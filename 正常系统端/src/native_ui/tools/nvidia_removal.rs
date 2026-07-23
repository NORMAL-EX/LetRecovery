//! Dedicated native NVIDIA-driver removal dialog preserving the pre-migration workflow.
//!
//! Targets come only from the host's Windows inventory. Hardware and the effective complete
//! removal scope are read-only; the dialog never performs removal itself.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, LBS_NOINTEGRALHEIGHT, LB_ADDSTRING,
    LB_RESETCONTENT, WM_SETFONT, WS_BORDER, WS_TABSTOP, WS_VSCROLL,
};

use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    arrange_field, measure_text, preferred_list_height, FieldArrangement, LayoutMetrics,
};
use super::super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::native_nvidia_removal::{
    removal_scope, validate_request, NvidiaHardwareReport, NvidiaRemovalRequest,
    NvidiaRemovalTarget,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaRemovalTargetOption {
    pub target: NvidiaRemovalTarget,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NvidiaRemovalDialogIntent {
    LoadHardwareReport,
    ReloadTargetsAndHardware,
    RequestConfirmation(NvidiaRemovalRequest),
    Close,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    targets: Vec<NvidiaRemovalTargetOption>,
    selected_target: Option<NvidiaRemovalTarget>,
    report: Option<NvidiaHardwareReport>,
    loading: bool,
    message: String,
}

#[derive(Clone, Copy)]
struct Controls {
    target_label: HWND,
    target_combo: HWND,
    hardware_label: HWND,
    hardware_list: HWND,
    hardware_value: HWND,
    scope_label: HWND,
    scope: HWND,
    warning: HWND,
    status: HWND,
}

pub struct NativeNvidiaRemovalDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: State,
    font: HFONT,
}

impl NativeNvidiaRemovalDialog {
    pub unsafe fn create(
        owner: HWND,
        targets: Vec<NvidiaRemovalTargetOption>,
    ) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("英伟达显卡驱动卸载"),
                title: crate::tr!("英伟达显卡驱动卸载"),
                description: crate::tr!("此工具用于卸载系统中的英伟达(NVIDIA)显卡驱动"),
                width: 700,
                height: 450,
                buttons: DialogButtons {
                    primary: crate::tr!("开始卸载"),
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

    /// Starts the legacy eager hardware load without touching hardware on the window thread.
    pub unsafe fn begin_initial_load(&mut self) -> NvidiaRemovalDialogIntent {
        self.state.loading = true;
        self.state.message = crate::tr!("正在加载硬件信息...");
        self.render_state();
        self.fit_and_layout();
        NvidiaRemovalDialogIntent::LoadHardwareReport
    }

    pub unsafe fn apply_targets(&mut self, targets: Vec<NvidiaRemovalTargetOption>) {
        let previous = self.state.selected_target.clone();
        self.state.targets = targets;
        self.state.selected_target = previous
            .filter(|selected| {
                self.state
                    .targets
                    .iter()
                    .any(|option| &option.target == selected)
            })
            .or_else(|| {
                self.state
                    .targets
                    .first()
                    .map(|option| option.target.clone())
            });
        refill_targets(self.controls.target_combo, &self.state);
        self.render_state();
        self.fit_and_layout();
    }

    /// Host hook for `CBN_SELCHANGE`. An empty selection is represented by `CB_ERR`; the popup
    /// contains only real inventory targets and never shows a misleading placeholder row.
    pub unsafe fn handle_target_changed(&mut self) {
        let index = SendMessageW(
            self.controls.target_combo,
            CB_GETCURSEL,
            WPARAM(0),
            LPARAM(0),
        )
        .0;
        self.state.selected_target = combo_inventory_index(index, self.state.targets.len())
            .and_then(|index| self.state.targets.get(index))
            .map(|option| option.target.clone());
        self.state.message.clear();
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn apply_hardware_report(&mut self, result: Result<NvidiaHardwareReport, String>) {
        self.state.loading = false;
        match result {
            Ok(report) => {
                self.state.message = if report.nvidia_device_count == 0 {
                    crate::tr!("当前系统未检测到英伟达显卡")
                } else {
                    String::new()
                };
                self.state.report = Some(report);
            }
            Err(error) => {
                self.state.report = None;
                self.state.message = crate::tr!("读取 NVIDIA 硬件信息失败: {}", error);
            }
        }
        refill_hardware(self.controls.hardware_list, self.state.report.as_ref());
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn set_busy(&mut self, message: String) {
        self.state.loading = true;
        self.state.message = message;
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn set_operation_result(&mut self, message: String) {
        self.state.loading = false;
        self.state.message = message;
        self.render_state();
        self.fit_and_layout();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<NvidiaRemovalDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Secondary => {
                self.state.loading = true;
                self.state.message = crate::tr!("正在加载硬件信息...");
                self.render_state();
                Some(NvidiaRemovalDialogIntent::ReloadTargetsAndHardware)
            }
            DialogResult::Cancel => Some(NvidiaRemovalDialogIntent::Close),
            DialogResult::Primary => match self.request() {
                Ok(request) => Some(NvidiaRemovalDialogIntent::RequestConfirmation(request)),
                Err(error) => {
                    self.state.message = error;
                    self.render_state();
                    None
                }
            },
        }
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let target_label_width = measure_text(
            self.shell.content(),
            self.font,
            &crate::tr!("目标系统:"),
            None,
        )
        .width;
        let models = self
            .state
            .report
            .as_ref()
            .map(nvidia_models)
            .unwrap_or_default();
        let hardware_text = models
            .first()
            .map(String::as_str)
            .unwrap_or_else(|| self.state.message.as_str());
        let hardware_value_height =
            measure_text(self.shell.content(), self.font, hardware_text, Some(width))
                .height
                .max(metrics.label_height);
        let scope = self.scope_text();
        let scope_height = measure_text(self.shell.content(), self.font, &scope, Some(width))
            .height
            .max(metrics.label_height);
        let warning = crate::tr!(
            "注意：卸载后显示可能切换到基本显示适配器，并且可能需要重启。请先保存工作并备份重要数据。"
        );
        let warning_height = measure_text(self.shell.content(), self.font, &warning, Some(width))
            .height
            .max(metrics.label_height);
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
            .max(metrics.label_height)
        };
        let layout = NvidiaContentLayout::calculate(
            width,
            dpi,
            target_label_width,
            models.len(),
            hardware_value_height,
            scope_height,
            warning_height,
            status_height,
        );
        self.shell
            .fit_content_height(logical_height(layout.content_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let (label_width, value_x, value_width) = field_geometry(layout.target, width);
        let _ = MoveWindow(
            self.controls.target_label,
            0,
            layout.target_label_y,
            label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.target_combo,
            value_x,
            layout.target_combo_y,
            value_width,
            scale(240, dpi),
            true,
        );
        let _ = MoveWindow(
            self.controls.hardware_label,
            0,
            layout.hardware_label_y,
            width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.hardware_list,
            0,
            layout.hardware_y,
            width,
            layout.hardware_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.hardware_value,
            0,
            layout.hardware_y,
            width,
            layout.hardware_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.scope_label,
            0,
            layout.scope_label_y,
            width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.scope,
            0,
            layout.scope_value_y,
            width,
            layout.scope_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.warning,
            0,
            layout.warning_y,
            width,
            layout.warning_height,
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

    fn request(&self) -> Result<NvidiaRemovalRequest, String> {
        let request = NvidiaRemovalRequest {
            target: self
                .state
                .selected_target
                .clone()
                .ok_or_else(|| crate::tr!("请先选择目标系统"))?,
        };
        validate_request(&request).map_err(|error| error.to_string())?;
        Ok(request)
    }

    fn scope_text(&self) -> String {
        self.state
            .selected_target
            .as_ref()
            .and_then(|target| removal_scope(target).ok())
            .unwrap_or_else(|| crate::tr!("选择目标系统后显示实际清理范围。"))
    }

    unsafe fn render_state(&self) {
        let _ = EnableWindow(self.controls.target_combo, !self.state.loading);
        self.shell
            .set_primary_enabled(self.request().is_ok() && !self.state.loading);
        let scope = self.scope_text();
        set_text(self.controls.scope, &scope);
        set_text(self.controls.status, &self.state.message);
        let models = self
            .state
            .report
            .as_ref()
            .map(nvidia_models)
            .unwrap_or_default();
        let show_list = models.len() > 1;
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            self.controls.hardware_list,
            if show_list {
                windows::Win32::UI::WindowsAndMessaging::SW_SHOW
            } else {
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE
            },
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            self.controls.hardware_value,
            if show_list {
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE
            } else {
                windows::Win32::UI::WindowsAndMessaging::SW_SHOW
            },
        );
        set_text(
            self.controls.hardware_value,
            models
                .first()
                .map(String::as_str)
                .unwrap_or_else(|| self.state.message.as_str()),
        );
    }

    unsafe fn apply_font_and_theme(&self) {
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        let palette = Palette::system();
        apply_control_theme(
            self.controls.target_combo,
            palette,
            NativeControlKind::Field,
        );
        apply_control_theme(
            self.controls.hardware_list,
            palette,
            NativeControlKind::List,
        );
    }

    fn controls(&self) -> [HWND; 9] {
        let controls = self.controls;
        [
            controls.target_label,
            controls.target_combo,
            controls.hardware_label,
            controls.hardware_list,
            controls.hardware_value,
            controls.scope_label,
            controls.scope,
            controls.warning,
            controls.status,
        ]
    }
}

impl Drop for NativeNvidiaRemovalDialog {
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
        target_label: child(parent, w!("STATIC"), &crate::tr!("目标系统:"), 0, 64_760)?,
        target_combo: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            64_761,
        )?,
        hardware_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("检测到的硬件信息:"),
            0,
            64_762,
        )?,
        hardware_list: child(
            parent,
            w!("LISTBOX"),
            "",
            WS_BORDER.0 as i32
                | WS_TABSTOP.0 as i32
                | WS_VSCROLL.0 as i32
                | LBS_NOINTEGRALHEIGHT
                | windows::Win32::UI::WindowsAndMessaging::LBS_NOSEL,
            64_763,
        )?,
        hardware_value: child(parent, w!("STATIC"), "", 0, 64_768)?,
        scope_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("实际清理范围:"),
            0,
            64_764,
        )?,
        scope: child(parent, w!("STATIC"), "", 0, 64_765)?,
        warning: child(
            parent,
            w!("STATIC"),
            &crate::tr!(
                "注意：卸载后显示可能切换到基本显示适配器，并且可能需要重启。请先保存工作并备份重要数据。"
            ),
            0,
            64_766,
        )?,
        status: child(parent, w!("STATIC"), "", 0, 64_767)?,
    })
}

unsafe fn refill_targets(control: HWND, state: &State) {
    let _ = SendMessageW(control, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let mut selected = NO_COMBO_SELECTION;
    for (index, option) in state.targets.iter().enumerate() {
        add_string(control, CB_ADDSTRING, &option.label);
        if state.selected_target.as_ref() == Some(&option.target) {
            selected = index;
        }
    }
    let _ = SendMessageW(control, CB_SETCURSEL, WPARAM(selected), LPARAM(0));
}

unsafe fn refill_hardware(control: HWND, report: Option<&NvidiaHardwareReport>) {
    let _ = SendMessageW(control, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    if let Some(report) = report {
        for row in report.rows.iter().filter(|row| row.is_nvidia) {
            add_string(control, LB_ADDSTRING, &row.value);
        }
    }
}

fn nvidia_models(report: &NvidiaHardwareReport) -> Vec<String> {
    report
        .rows
        .iter()
        .filter(|row| row.is_nvidia)
        .map(|row| row.value.clone())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NvidiaContentLayout {
    target: FieldArrangement,
    target_label_y: i32,
    target_combo_y: i32,
    hardware_label_y: i32,
    hardware_y: i32,
    hardware_height: i32,
    scope_label_y: i32,
    scope_value_y: i32,
    scope_height: i32,
    warning_y: i32,
    warning_height: i32,
    status_y: i32,
    status_height: i32,
    content_height: i32,
}

impl NvidiaContentLayout {
    #[allow(clippy::too_many_arguments)]
    fn calculate(
        width: i32,
        dpi: u32,
        target_label_width: i32,
        device_count: usize,
        hardware_value_height: i32,
        scope_height: i32,
        warning_height: i32,
        status_height: i32,
    ) -> Self {
        let metrics = LayoutMetrics::for_dpi(dpi);
        let target = arrange_field(width, target_label_width, scale(300, dpi), dpi);
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
        let hardware_label_y = target_bottom + metrics.section_gap;
        let hardware_y = hardware_label_y + metrics.label_height + metrics.tight_gap;
        let hardware_height = if device_count > 1 {
            (preferred_list_height(device_count, dpi, 3, 8) - metrics.list_row_height)
                .max(metrics.list_row_height * 3)
        } else {
            hardware_value_height
        };
        let scope_label_y = hardware_y + hardware_height + metrics.section_gap;
        let scope_value_y = scope_label_y + metrics.label_height + metrics.tight_gap;
        let warning_y = scope_value_y + scope_height + metrics.section_gap;
        let status_y = warning_y
            + warning_height
            + if status_height > 0 {
                metrics.control_gap
            } else {
                0
            };
        Self {
            target,
            target_label_y,
            target_combo_y,
            hardware_label_y,
            hardware_y,
            hardware_height,
            scope_label_y,
            scope_value_y,
            scope_height,
            warning_y,
            warning_height,
            status_y,
            status_height,
            content_height: status_y + status_height,
        }
    }
}

unsafe fn add_string(control: HWND, message: u32, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(control, message, WPARAM(0), LPARAM(value.as_ptr() as isize));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_has_no_fabricated_target_before_inventory_arrives() {
        let state = State {
            targets: vec![NvidiaRemovalTargetOption {
                target: NvidiaRemovalTarget::CurrentSystem,
                label: "Current system".to_owned(),
            }],
            ..Default::default()
        };
        assert!(state.selected_target.is_none());
    }

    #[test]
    fn changing_inventory_discards_a_target_that_no_longer_exists() {
        let mut state = State {
            selected_target: Some(NvidiaRemovalTarget::OfflineWindows("D:".to_owned())),
            ..Default::default()
        };
        state.targets = vec![NvidiaRemovalTargetOption {
            target: NvidiaRemovalTarget::CurrentSystem,
            label: "Current system".to_owned(),
        }];
        state.selected_target = state.selected_target.filter(|selected| {
            state
                .targets
                .iter()
                .any(|option| &option.target == selected)
        });
        assert!(state.selected_target.is_none());
    }

    #[test]
    fn single_nvidia_gpu_uses_compact_read_only_text() {
        let report = NvidiaHardwareReport {
            rows: vec![crate::core::native_nvidia_removal::NvidiaHardwareRow {
                item: "NVIDIA GPU".to_owned(),
                value: "GeForce RTX".to_owned(),
                is_nvidia: true,
            }],
            nvidia_device_count: 1,
        };
        assert_eq!(nvidia_models(&report), vec!["GeForce RTX"]);
    }

    #[test]
    fn hardware_layout_grows_only_for_real_multiple_gpu_rows() {
        let single = NvidiaContentLayout::calculate(620, 96, 80, 1, 20, 20, 40, 0);
        let multiple = NvidiaContentLayout::calculate(620, 96, 80, 8, 20, 20, 40, 0);
        assert_eq!(single.hardware_height, 20);
        assert_eq!(multiple.hardware_height, 178);
        assert_eq!(multiple.content_height - single.content_height, 158);
        assert!(matches!(single.target, FieldArrangement::Inline { .. }));
    }
}
