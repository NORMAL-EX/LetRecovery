//! Dedicated native dialog for importing the packaged storage-controller drivers.
//!
//! The dialog restores the legacy one-field workflow: select one enumerated offline Windows
//! target, then request explicit confirmation. It has no directory editor, browse button, or
//! recursion option and performs no filesystem access, DISM call, or driver installation.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, CBS_DROPDOWNLIST,
    CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, SW_HIDE, SW_SHOW, WM_SETFONT,
    WS_TABSTOP,
};

use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{arrange_field, measure_text, FieldArrangement, LayoutMetrics};
use super::super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::native_storage_driver::{StorageDriverImportRequest, StorageDriverTarget};

const ID_TARGET_LABEL: u16 = 64_720;
pub const ID_TARGET: u16 = 64_721;
const ID_STATUS: u16 = 64_722;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageDriverDialogState {
    pub loading: bool,
    pub targets: Vec<StorageDriverTarget>,
    selected_target: Option<String>,
    pub message: String,
}

impl Default for StorageDriverDialogState {
    fn default() -> Self {
        Self {
            loading: true,
            targets: Vec::new(),
            selected_target: None,
            message: crate::tr!("正在检测Windows分区..."),
        }
    }
}

impl StorageDriverDialogState {
    pub fn selected_target(&self) -> Option<&str> {
        self.selected_target.as_deref()
    }

    pub fn begin_loading(&mut self) {
        self.loading = true;
        self.targets.clear();
        self.selected_target = None;
        self.message = crate::tr!("正在检测Windows分区...");
    }

    pub fn apply_targets(&mut self, result: Result<Vec<StorageDriverTarget>, String>) {
        self.loading = false;
        match result {
            Ok(targets) => {
                self.targets = sanitize_targets(targets);
                self.selected_target = self
                    .selected_target
                    .take()
                    .filter(|selected| self.targets.iter().any(|target| &target.root == selected))
                    .or_else(|| self.targets.first().map(|target| target.root.clone()));
                self.message = if self.targets.is_empty() {
                    crate::tr!("未找到包含 Windows 系统的分区")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.targets.clear();
                self.selected_target = None;
                self.message = crate::tr!("加载失败：{}", error);
            }
        }
    }

    pub fn select(&mut self, target: Option<&str>) {
        self.selected_target = target.and_then(|target| {
            self.targets
                .iter()
                .find(|candidate| candidate.root == target)
                .map(|candidate| candidate.root.clone())
        });
    }

    pub fn request(&self) -> Option<StorageDriverImportRequest> {
        (!self.loading)
            .then(|| self.selected_target.clone())
            .flatten()
            .map(|target| StorageDriverImportRequest { target })
    }
}

fn sanitize_targets(targets: Vec<StorageDriverTarget>) -> Vec<StorageDriverTarget> {
    let mut result = Vec::new();
    for mut target in targets {
        let Some(root) = normalize_drive(&target.root) else {
            continue;
        };
        if root == "X:"
            || result
                .iter()
                .any(|existing: &StorageDriverTarget| existing.root == root)
        {
            continue;
        }
        target.root = root;
        if target.label.trim().is_empty() {
            target.label = target.root.clone();
        }
        result.push(target);
    }
    result
}

fn normalize_drive(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches(['\\', '/']);
    match value.as_bytes() {
        [letter] if letter.is_ascii_alphabetic() => {
            Some(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            Some(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageDriverDialogIntent {
    RequestConfirmation(StorageDriverImportRequest),
    Close,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StorageDriverLayout {
    field: FieldArrangement,
    label_y: i32,
    combo_y: i32,
    status_y: i32,
    status_height: i32,
    content_height: i32,
}

impl StorageDriverLayout {
    fn calculate(width: i32, dpi: u32, label_width: i32, status_height: i32) -> Self {
        let width = width.max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let field = arrange_field(width, label_width, scale(260, dpi), dpi);
        let (label_y, combo_y, field_bottom) = match field {
            FieldArrangement::Inline { .. } => (
                ((metrics.field_height - metrics.label_height) / 2).max(0),
                0,
                metrics.field_height,
            ),
            FieldArrangement::Stacked => (
                0,
                metrics.label_height + metrics.tight_gap,
                metrics.label_height + metrics.tight_gap + metrics.field_height,
            ),
        };
        let status_y = field_bottom
            + if status_height > 0 {
                metrics.control_gap
            } else {
                0
            };
        Self {
            field,
            label_y,
            combo_y,
            status_y,
            status_height,
            content_height: status_y + status_height,
        }
    }
}

#[derive(Clone, Copy)]
struct StorageDriverControls {
    target_label: HWND,
    target: HWND,
    status: HWND,
}

pub struct NativeStorageDriverDialog {
    pub shell: DialogShell,
    controls: StorageDriverControls,
    state: StorageDriverDialogState,
    font: HFONT,
}

impl NativeStorageDriverDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("导入硬盘控制器驱动"),
                title: crate::tr!("导入硬盘控制器驱动"),
                description: crate::tr!(
                    "将 Intel VMD / Apple SSD / Visior 等硬盘控制器驱动导入到离线系统"
                ),
                width: 560,
                height: 280,
                buttons: DialogButtons {
                    primary: crate::tr!("导入驱动"),
                    secondary: None,
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
        let dpi = GetDpiForWindow(shell.hwnd()).max(96);
        let face = wide("Microsoft YaHei UI");
        let font = CreateFontW(
            -scale(14, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            PCWSTR(face.as_ptr()),
        );
        let controls = StorageDriverControls {
            target_label: child(
                shell.content(),
                w!("STATIC"),
                &crate::tr!("目标分区:"),
                0,
                ID_TARGET_LABEL,
            )?,
            target: child(
                shell.content(),
                w!("COMBOBOX"),
                "",
                CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
                ID_TARGET,
            )?,
            status: child(shell.content(), w!("STATIC"), "", 0, ID_STATUS)?,
        };
        let mut dialog = Self {
            shell,
            controls,
            state: StorageDriverDialogState::default(),
            font,
        };
        dialog.apply_font_and_theme();
        dialog.render_state();
        dialog.fit_and_layout();
        Ok(dialog)
    }

    pub fn state(&self) -> &StorageDriverDialogState {
        &self.state
    }

    pub fn owns_target(&self, control: HWND) -> bool {
        control == self.controls.target
    }

    /// Called by the host for `CBN_SELCHANGE` from the target combobox.
    pub unsafe fn handle_target_changed(&mut self) {
        let index = SendMessageW(self.controls.target, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let selected = combo_inventory_index(index, self.state.targets.len())
            .and_then(|index| self.state.targets.get(index))
            .map(|target| target.root.clone());
        self.state.select(selected.as_deref());
        self.render_enablement();
    }

    pub unsafe fn set_targets(&mut self, result: Result<Vec<StorageDriverTarget>, String>) {
        self.state.apply_targets(result);
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn set_loading(&mut self) {
        self.state.begin_loading();
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<StorageDriverDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Primary => match self.state.request() {
                Some(request) => Some(StorageDriverDialogIntent::RequestConfirmation(request)),
                None => {
                    self.state.message = crate::tr!("请先选择目标分区");
                    self.render_enablement();
                    None
                }
            },
            DialogResult::Cancel => Some(StorageDriverDialogIntent::Close),
            DialogResult::Secondary => None,
        }
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let label_width = measure_text(
            self.shell.content(),
            self.font,
            &crate::tr!("目标分区:"),
            None,
        )
        .width;
        let status_height = if self.state.message.is_empty() {
            0
        } else {
            measure_text(
                self.shell.content(),
                self.font,
                &self.state.message,
                Some(width),
            )
            .height
            .max(LayoutMetrics::for_dpi(dpi).label_height)
        };
        let layout = StorageDriverLayout::calculate(width, dpi, label_width, status_height);
        self.shell
            .fit_content_height(logical_height(layout.content_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let (label_width, combo_x, combo_width) = match layout.field {
            FieldArrangement::Inline {
                label_width,
                control_x,
                control_width,
            } => (label_width, control_x, control_width),
            FieldArrangement::Stacked => (width, 0, width),
        };
        let _ = MoveWindow(
            self.controls.target_label,
            0,
            layout.label_y,
            label_width,
            LayoutMetrics::for_dpi(dpi).label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.target,
            combo_x,
            layout.combo_y,
            combo_width,
            scale(240, dpi),
            true,
        );
        let _ = MoveWindow(
            self.controls.status,
            0,
            layout.status_y,
            width,
            layout.status_height,
            true,
        );
    }

    unsafe fn render_state(&self) {
        let _ = SendMessageW(self.controls.target, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for target in &self.state.targets {
            add_combo_item(self.controls.target, &target.label);
        }
        let selected = self
            .state
            .selected_target()
            .and_then(|selected| {
                self.state
                    .targets
                    .iter()
                    .position(|target| target.root == selected)
            })
            .map_or(NO_COMBO_SELECTION, |index| index);
        let _ = SendMessageW(
            self.controls.target,
            CB_SETCURSEL,
            WPARAM(selected),
            LPARAM(0),
        );
        let _ = EnableWindow(
            self.controls.target,
            !self.state.loading && !self.state.targets.is_empty(),
        );
        self.render_enablement();
    }

    unsafe fn render_enablement(&self) {
        set_text(self.controls.status, &self.state.message);
        let _ = ShowWindow(
            self.controls.status,
            if self.state.message.is_empty() {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        self.shell
            .set_primary_enabled(self.state.request().is_some());
    }

    unsafe fn apply_font_and_theme(&self) {
        for control in [
            self.controls.target_label,
            self.controls.target,
            self.controls.status,
        ] {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        apply_control_theme(
            self.controls.target,
            Palette::system(),
            NativeControlKind::Field,
        );
    }
}

impl Drop for NativeStorageDriverDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn add_combo_item(combo: HWND, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(
        combo,
        CB_ADDSTRING,
        WPARAM(0),
        LPARAM(value.as_ptr() as isize),
    );
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> Vec<StorageDriverTarget> {
        vec![
            StorageDriverTarget::new("D:", "D: [Windows 11 24H2] [x64]"),
            StorageDriverTarget::new("E:", "E: [Windows 10 22H2] [x64]"),
        ]
    }

    #[test]
    fn inventory_selects_the_preferred_first_windows_target() {
        let mut state = StorageDriverDialogState::default();
        state.apply_targets(Ok(targets()));
        assert_eq!(state.selected_target(), Some("D:"));
        state.select(Some("E:"));
        assert_eq!(
            state.request(),
            Some(StorageDriverImportRequest {
                target: "E:".into()
            })
        );
    }

    #[test]
    fn refresh_discards_stale_selection_and_errors_fail_closed() {
        let mut state = StorageDriverDialogState::default();
        state.apply_targets(Ok(targets()));
        state.select(Some("D:"));
        state.apply_targets(Ok(vec![StorageDriverTarget::new(
            "E:",
            "E: [Windows 10] [x64]",
        )]));
        assert_eq!(state.selected_target(), Some("E:"));
        state.apply_targets(Err("inventory failed".into()));
        assert!(state.targets.is_empty());
        assert_eq!(state.request(), None);
    }

    #[test]
    fn presentation_filters_protected_invalid_and_duplicate_roots() {
        let targets = sanitize_targets(vec![
            StorageDriverTarget::new("C:", "current"),
            StorageDriverTarget::new("X:", "PE"),
            StorageDriverTarget::new("d", "offline"),
            StorageDriverTarget::new("D:\\", "duplicate"),
            StorageDriverTarget::new("path", "invalid"),
        ]);
        assert_eq!(
            targets,
            [
                StorageDriverTarget::new("C:", "current"),
                StorageDriverTarget::new("D:", "offline")
            ]
        );
    }

    #[test]
    fn responsive_layout_keeps_only_target_control_and_status() {
        let normal = StorageDriverLayout::calculate(500, 96, 80, 20);
        assert!(matches!(normal.field, FieldArrangement::Inline { .. }));
        assert_eq!(normal.content_height, normal.status_y + 20);

        let narrow = StorageDriverLayout::calculate(300, 96, 120, 0);
        assert_eq!(narrow.field, FieldArrangement::Stacked);
        assert_eq!(narrow.content_height, narrow.status_y);
    }
}
