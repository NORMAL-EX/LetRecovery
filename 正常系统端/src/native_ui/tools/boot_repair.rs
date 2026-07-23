//! Dedicated native UI for the legacy one-click boot-repair workflow.
//!
//! It only selects one detected Windows partition, shows its version/architecture, and produces
//! refresh/confirmation/close intents. No UEFI/Legacy selector or boot-writing operation exists.

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
use crate::core::native_boot_repair::{BootRepairRequest, BootRepairTarget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootRepairDialogIntent {
    Refresh,
    RequestConfirmation(BootRepairRequest),
    Close,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    targets: Vec<BootRepairTarget>,
    selected_partition: Option<String>,
    loading: bool,
    running: bool,
    status: String,
}

#[derive(Clone, Copy)]
struct Controls {
    target_label: HWND,
    target_combo: HWND,
    version_label: HWND,
    version_value: HWND,
    architecture_label: HWND,
    architecture_value: HWND,
    status: HWND,
}

pub struct NativeBootRepairDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: State,
    font: HFONT,
}

impl NativeBootRepairDialog {
    pub unsafe fn create(
        owner: HWND,
        targets: Vec<BootRepairTarget>,
    ) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("一键修复引导"),
                title: crate::tr!("一键修复引导"),
                description: crate::tr!("修复Windows系统的启动引导"),
                width: 610,
                height: 420,
                buttons: DialogButtons {
                    primary: crate::tr!("开始修复"),
                    secondary: Some(crate::tr!("刷新")),
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
        let controls = create_controls(shell.content())?;
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
        let mut dialog = Self {
            shell,
            controls,
            state: State::default(),
            font,
        };
        dialog.apply_font_and_theme();
        dialog.apply_targets(targets);
        dialog.fit_and_layout();
        Ok(dialog)
    }

    pub fn owns_target_combo(&self, control: HWND) -> bool {
        control == self.controls.target_combo
    }

    /// Replaces the detected inventory. Refresh preserves an existing stable selection; otherwise
    /// the inventory's preferred first target is selected. The host orders the online system first
    /// on desktop and the first offline Windows target first in PE.
    pub unsafe fn apply_targets(&mut self, targets: Vec<BootRepairTarget>) {
        let previous = self.state.selected_partition.clone();
        self.state.targets = targets;
        self.state.selected_partition = previous
            .and_then(|selected| {
                self.state
                    .targets
                    .iter()
                    .find(|target| target.partition.eq_ignore_ascii_case(&selected))
                    .map(|target| target.partition.clone())
            })
            .or_else(|| {
                self.state
                    .targets
                    .first()
                    .map(|target| target.partition.clone())
            });
        self.state.loading = false;
        self.state.running = false;
        self.state.status = if self.state.targets.is_empty() {
            crate::tr!("未检测到包含Windows系统的分区")
        } else {
            String::new()
        };
        refill_targets(self.controls.target_combo, &self.state);
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn set_loading(&mut self) {
        self.state.loading = true;
        self.state.status = crate::tr!("正在检测Windows分区...");
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn set_running(&mut self) {
        self.state.running = true;
        self.state.status = crate::tr!("正在修复引导...");
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn set_status(&mut self, status: impl Into<String>) {
        self.state.loading = false;
        self.state.running = false;
        self.state.status = status.into();
        self.render_state();
        self.fit_and_layout();
    }

    /// Host hook for `CBN_SELCHANGE`.
    pub unsafe fn handle_target_changed(&mut self) {
        let index = SendMessageW(
            self.controls.target_combo,
            CB_GETCURSEL,
            WPARAM(0),
            LPARAM(0),
        )
        .0;
        self.state.selected_partition = combo_inventory_index(index, self.state.targets.len())
            .and_then(|index| self.state.targets.get(index))
            .map(|target| target.partition.clone());
        self.state.status.clear();
        self.render_state();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<BootRepairDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Cancel => Some(BootRepairDialogIntent::Close),
            DialogResult::Secondary if !self.state.running => {
                self.set_loading();
                Some(BootRepairDialogIntent::Refresh)
            }
            DialogResult::Secondary => None,
            DialogResult::Primary => self
                .request()
                .map(BootRepairDialogIntent::RequestConfirmation),
        }
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let target_label_width = measure_text(
            self.shell.content(),
            self.font,
            &crate::tr!("选择目标系统分区:"),
            None,
        )
        .width;
        let detail_label_width = [crate::tr!("Windows版本:"), crate::tr!("系统架构:")]
            .iter()
            .map(|label| measure_text(self.shell.content(), self.font, label, None).width)
            .max()
            .unwrap_or_default();
        let status_height = if self.state.status.is_empty() {
            0
        } else {
            measure_text(
                self.shell.content(),
                self.font,
                &self.state.status,
                Some(width),
            )
            .height
            .max(LayoutMetrics::for_dpi(dpi).label_height)
        };
        let layout = BootRepairContentLayout::calculate(
            width,
            dpi,
            target_label_width,
            detail_label_width,
            status_height,
        );
        self.shell
            .fit_content_height(logical_height(layout.content_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let (target_label_width, target_x, target_width) = field_geometry(layout.target, width);
        let _ = MoveWindow(
            self.controls.target_label,
            0,
            layout.target_label_y,
            target_label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.target_combo,
            target_x,
            layout.target_combo_y,
            target_width,
            scale(240, dpi),
            true,
        );
        let (detail_label_width, value_x, value_width) = field_geometry(layout.details, width);
        let _ = MoveWindow(
            self.controls.version_label,
            0,
            layout.version_label_y,
            detail_label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.version_value,
            value_x,
            layout.version_value_y,
            value_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.architecture_label,
            0,
            layout.architecture_label_y,
            detail_label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.architecture_value,
            value_x,
            layout.architecture_value_y,
            value_width,
            metrics.label_height,
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

    fn request(&self) -> Option<BootRepairRequest> {
        (!self.state.loading && !self.state.running)
            .then(|| self.state.selected_partition.clone())
            .flatten()
            .map(|target_partition| BootRepairRequest { target_partition })
    }

    unsafe fn render_state(&self) {
        let selected = self
            .state
            .selected_partition
            .as_ref()
            .and_then(|partition| {
                self.state
                    .targets
                    .iter()
                    .find(|target| target.partition.eq_ignore_ascii_case(partition))
            });
        set_text(
            self.controls.version_value,
            selected.map_or("-", |target| target.windows_version.as_str()),
        );
        set_text(
            self.controls.architecture_value,
            selected.map_or("-", |target| target.architecture.as_str()),
        );
        set_text(self.controls.status, &self.state.status);
        let _ = ShowWindow(
            self.controls.status,
            if self.state.status.is_empty() {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        let _ = EnableWindow(
            self.controls.target_combo,
            !self.state.loading && !self.state.running,
        );
        self.shell.set_primary_enabled(self.request().is_some());
    }

    unsafe fn apply_font_and_theme(&self) {
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        apply_control_theme(
            self.controls.target_combo,
            Palette::system(),
            NativeControlKind::Field,
        );
    }

    fn controls(&self) -> [HWND; 7] {
        let c = self.controls;
        [
            c.target_label,
            c.target_combo,
            c.version_label,
            c.version_value,
            c.architecture_label,
            c.architecture_value,
            c.status,
        ]
    }
}

impl Drop for NativeBootRepairDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<Controls> {
    Ok(Controls {
        target_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("选择目标系统分区:"),
            0,
            64_730,
        )?,
        target_combo: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            64_731,
        )?,
        version_label: child(parent, w!("STATIC"), &crate::tr!("Windows版本:"), 0, 64_732)?,
        version_value: child(parent, w!("STATIC"), "-", 0, 64_733)?,
        architecture_label: child(parent, w!("STATIC"), &crate::tr!("系统架构:"), 0, 64_734)?,
        architecture_value: child(parent, w!("STATIC"), "-", 0, 64_735)?,
        status: child(parent, w!("STATIC"), "", 0, 64_736)?,
    })
}

unsafe fn refill_targets(control: HWND, state: &State) {
    let _ = SendMessageW(control, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let mut selected = NO_COMBO_SELECTION;
    for (index, target) in state.targets.iter().enumerate() {
        add_string(control, &target.display_label());
        if state
            .selected_partition
            .as_ref()
            .is_some_and(|selected| selected.eq_ignore_ascii_case(&target.partition))
        {
            selected = index;
        }
    }
    let _ = SendMessageW(control, CB_SETCURSEL, WPARAM(selected), LPARAM(0));
}

unsafe fn add_string(control: HWND, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(
        control,
        CB_ADDSTRING,
        WPARAM(0),
        LPARAM(value.as_ptr() as isize),
    );
}

unsafe fn set_text(control: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(control, PCWSTR(value.as_ptr()));
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

fn field_geometry(field: FieldArrangement, width: i32) -> (i32, i32, i32) {
    match field {
        FieldArrangement::Inline {
            label_width,
            control_x,
            control_width,
        } => (label_width, control_x, control_width),
        FieldArrangement::Stacked => (width, 0, width),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BootRepairContentLayout {
    target: FieldArrangement,
    details: FieldArrangement,
    target_label_y: i32,
    target_combo_y: i32,
    version_label_y: i32,
    version_value_y: i32,
    architecture_label_y: i32,
    architecture_value_y: i32,
    status_y: i32,
    status_height: i32,
    content_height: i32,
}

impl BootRepairContentLayout {
    fn calculate(
        width: i32,
        dpi: u32,
        target_label_width: i32,
        detail_label_width: i32,
        status_height: i32,
    ) -> Self {
        let metrics = LayoutMetrics::for_dpi(dpi);
        let target = arrange_field(width, target_label_width, scale(260, dpi), dpi);
        let (target_label_y, target_combo_y, target_bottom) = match target {
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
        let details = arrange_field(width, detail_label_width, scale(180, dpi), dpi);
        let details_y = target_bottom + metrics.section_gap;
        let (
            version_label_y,
            version_value_y,
            architecture_label_y,
            architecture_value_y,
            details_bottom,
        ) = match details {
            FieldArrangement::Inline { .. } => {
                let architecture_y = details_y + metrics.label_height + metrics.control_gap;
                (
                    details_y,
                    details_y,
                    architecture_y,
                    architecture_y,
                    architecture_y + metrics.label_height,
                )
            }
            FieldArrangement::Stacked => {
                let version_value_y = details_y + metrics.label_height + metrics.tight_gap;
                let architecture_label_y =
                    version_value_y + metrics.label_height + metrics.control_gap;
                let architecture_value_y =
                    architecture_label_y + metrics.label_height + metrics.tight_gap;
                (
                    details_y,
                    version_value_y,
                    architecture_label_y,
                    architecture_value_y,
                    architecture_value_y + metrics.label_height,
                )
            }
        };
        let status_y = details_bottom
            + if status_height > 0 {
                metrics.control_gap
            } else {
                0
            };
        Self {
            target,
            details,
            target_label_y,
            target_combo_y,
            version_label_y,
            version_value_y,
            architecture_label_y,
            architecture_value_y,
            status_y,
            status_height,
            content_height: status_y + status_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(partition: &str) -> BootRepairTarget {
        BootRepairTarget {
            partition: partition.to_owned(),
            windows_version: "Windows 11".to_owned(),
            architecture: "x64".to_owned(),
        }
    }

    #[test]
    fn initial_state_never_preselects_a_target() {
        let state = State {
            targets: vec![target("D:")],
            ..Default::default()
        };
        assert!(state.selected_partition.is_none());
    }

    #[test]
    fn refresh_drops_a_selection_that_is_no_longer_detected() {
        let mut state = State {
            targets: vec![target("D:")],
            selected_partition: Some("D:".to_owned()),
            ..Default::default()
        };
        state.targets = vec![target("E:")];
        state.selected_partition = state.selected_partition.filter(|selected| {
            state
                .targets
                .iter()
                .any(|candidate| candidate.partition.eq_ignore_ascii_case(selected))
        });
        assert!(state.selected_partition.is_none());
    }

    #[test]
    fn compact_layout_keeps_details_and_status_contiguous() {
        let layout = BootRepairContentLayout::calculate(540, 96, 120, 100, 20);
        assert!(matches!(layout.target, FieldArrangement::Inline { .. }));
        assert!(matches!(layout.details, FieldArrangement::Inline { .. }));
        assert_eq!(layout.content_height, layout.status_y + 20);

        let narrow = BootRepairContentLayout::calculate(300, 96, 140, 140, 0);
        assert_eq!(narrow.target, FieldArrangement::Stacked);
        assert_eq!(narrow.details, FieldArrangement::Stacked);
    }
}
