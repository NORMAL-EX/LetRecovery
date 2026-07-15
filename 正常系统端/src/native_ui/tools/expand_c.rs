//! Dedicated native dialog for the legacy "lossless expand C:" toolbox entry.
//!
//! This module is presentation and intent only.  It never enumerates disks, writes the expand
//! configuration, installs a PE boot entry, moves a partition, or restarts the computer.  The
//! host must obtain [`ExpandCAnalysis`] through a read-only controller, show a separate explicit
//! confirmation for [`ExpandCRequest`], then enter the existing typed PE handoff boundary.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, RedrawWindow, HFONT, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible, MoveWindow, SendMessageW,
    SetWindowTextW, ShowWindow, ES_AUTOHSCROLL, SW_HIDE, SW_SHOW, WM_SETFONT, WS_TABSTOP,
};

use super::super::controls::{child, wide};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{measure_text, LayoutMetrics};
use super::super::theme::{apply_control_theme, apply_trackbar_theme, NativeControlKind, Palette};

const ID_CURRENT_SIZE: u16 = 64_600;
const ID_USED_SIZE: u16 = 64_601;
const ID_FREE_SIZE: u16 = 64_602;
const ID_MAX_SIZE: u16 = 64_603;
const ID_TARGET_SIZE: u16 = 64_604;
const ID_TARGET_SLIDER: u16 = 64_605;
const ID_RANGE: u16 = 64_606;
const ID_MOVE_WARNING: u16 = 64_607;
const ID_STATUS: u16 = 64_608;

const TBM_GETPOS: u32 = 0x0400;
const TBM_SETPOS: u32 = 0x0405;
const TBM_SETRANGEMIN: u32 = 0x0407;
const TBM_SETRANGEMAX: u32 = 0x0408;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpandCAnalysis {
    pub found: bool,
    pub current_size_mb: u64,
    pub used_mb: u64,
    pub free_mb: u64,
    pub max_size_mb: u64,
    /// Maximum final size which only consumes already-adjacent unallocated space.
    pub no_move_max_mb: u64,
    pub can_expand: bool,
    pub reason: String,
}

impl From<crate::core::native_expand_c_controller::NativeExpandCAnalysis> for ExpandCAnalysis {
    fn from(value: crate::core::native_expand_c_controller::NativeExpandCAnalysis) -> Self {
        Self {
            found: value.found,
            current_size_mb: value.current_size_mb,
            used_mb: value.used_mb,
            free_mb: value.free_mb,
            max_size_mb: value.max_size_mb,
            no_move_max_mb: value.no_move_max_mb,
            can_expand: value.can_expand,
            reason: value.reason,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExpandCDialogState {
    pub loading: bool,
    pub executing: bool,
    pub message: String,
    pub analysis: ExpandCAnalysis,
    pub target_size_text: String,
    pub target_size_mb: u64,
}

impl ExpandCDialogState {
    /// Expansion never shrinks C:.  The used-space margin is retained from the legacy dialog.
    pub fn min_target_mb(&self) -> u64 {
        self.analysis
            .current_size_mb
            .max(self.analysis.used_mb.saturating_add(1024))
    }

    pub fn apply_analysis(&mut self, analysis: ExpandCAnalysis) {
        self.loading = false;
        self.executing = false;
        self.message = if !analysis.found {
            crate::tr!("未找到当前系统 C 盘")
        } else if !analysis.can_expand {
            if analysis.reason.is_empty() {
                crate::tr!("C 盘后方没有可用于扩容的空间")
            } else {
                analysis.reason.clone()
            }
        } else {
            String::new()
        };
        self.target_size_mb = analysis.max_size_mb;
        self.target_size_text = format_gb_value(analysis.max_size_mb);
        self.analysis = analysis;
    }

    /// Synchronizes the typed target with the native trackbar's absolute tenth-GB position.
    ///
    /// `TBM_GETPOS` reports the value configured by `TBM_SETRANGEMIN/MAX`, not an offset from
    /// zero. Clamp defensively to the analyzed range so an early/default notification cannot turn
    /// a 300 GB minimum into 0 GB while controls are being initialized.
    fn apply_slider_position(&mut self, position_tenths: i32) {
        let minimum = tenth_gb(self.min_target_mb());
        let maximum = tenth_gb(self.analysis.max_size_mb).max(minimum);
        let position_tenths = position_tenths.clamp(minimum, maximum);
        self.target_size_mb = mb_from_tenth_gb(position_tenths);
        self.target_size_text = format!("{:.1}", f64::from(position_tenths) / 10.0);
    }

    pub fn request(&self) -> Result<ExpandCRequest, ExpandCValidationError> {
        if self.loading || self.executing || !self.analysis.found || !self.analysis.can_expand {
            return Err(ExpandCValidationError::AnalysisUnavailable);
        }
        let target_gb = self
            .target_size_text
            .trim()
            .parse::<f64>()
            .map_err(|_| ExpandCValidationError::InvalidTarget)?;
        if !target_gb.is_finite() || target_gb <= 0.0 {
            return Err(ExpandCValidationError::InvalidTarget);
        }
        let target_size_mb = (target_gb * 1024.0).round() as u64;
        let minimum = self.min_target_mb();
        let maximum = self.analysis.max_size_mb;
        if target_size_mb < minimum || target_size_mb > maximum {
            return Err(ExpandCValidationError::OutsideRange {
                target_size_mb,
                minimum_mb: minimum,
                maximum_mb: maximum,
            });
        }
        Ok(ExpandCRequest {
            target_size_mb,
            use_maximum: target_size_mb >= maximum,
            requires_partition_move: self.analysis.no_move_max_mb > 0
                && target_size_mb > self.analysis.no_move_max_mb,
            analyzed_current_size_mb: self.analysis.current_size_mb,
            analyzed_max_size_mb: maximum,
            analyzed_no_move_max_mb: self.analysis.no_move_max_mb,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandCRequest {
    pub target_size_mb: u64,
    /// The legacy PE configuration encodes "maximum available" as zero.  The execution
    /// controller, not this UI module, owns that compatibility conversion.
    pub use_maximum: bool,
    pub requires_partition_move: bool,
    /// Analysis snapshot retained so the execution controller can reject a changed layout.
    pub analyzed_current_size_mb: u64,
    pub analyzed_max_size_mb: u64,
    pub analyzed_no_move_max_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandCValidationError {
    AnalysisUnavailable,
    InvalidTarget,
    OutsideRange {
        target_size_mb: u64,
        minimum_mb: u64,
        maximum_mb: u64,
    },
}

impl std::fmt::Display for ExpandCValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnalysisUnavailable => {
                formatter.write_str(&crate::tr!("尚未获得可用的 C 盘扩容分析结果"))
            }
            Self::InvalidTarget => formatter.write_str(&crate::tr!("请输入有效的目标大小")),
            Self::OutsideRange {
                minimum_mb,
                maximum_mb,
                ..
            } => write!(
                formatter,
                "{}",
                crate::tr!(
                    "目标大小必须在 {} GB 到 {} GB 之间",
                    format_gb_value(*minimum_mb),
                    format_gb_value(*maximum_mb)
                )
            ),
        }
    }
}

impl std::error::Error for ExpandCValidationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandCDialogIntent {
    Analyze,
    RequestConfirmation(ExpandCRequest),
    Close,
}

#[derive(Clone, Copy)]
struct ExpandCControls {
    current_label: HWND,
    current_value: HWND,
    used_label: HWND,
    used_value: HWND,
    free_label: HWND,
    free_value: HWND,
    max_label: HWND,
    max_value: HWND,
    target_label: HWND,
    target_edit: HWND,
    target_unit: HWND,
    slider: HWND,
    range: HWND,
    safety_note: HWND,
    move_warning: HWND,
    status: HWND,
}

pub struct NativeExpandCDialog {
    pub shell: DialogShell,
    controls: ExpandCControls,
    state: ExpandCDialogState,
    font: HFONT,
}

impl NativeExpandCDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("无损扩大C盘"),
                title: crate::tr!("无损扩大C盘"),
                description: crate::tr!("分析当前系统盘布局，并在确认后准备 WinPE 扩容任务。"),
                width: 610,
                height: 550,
                buttons: DialogButtons {
                    primary: crate::tr!("开始扩容"),
                    secondary: Some(crate::tr!("重新分析")),
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
        let controls = create_controls(shell.content())?;
        let mut dialog = Self {
            shell,
            controls,
            state: ExpandCDialogState {
                loading: true,
                message: crate::tr!("正在分析 C 盘可扩容空间..."),
                ..Default::default()
            },
            font,
        };
        dialog.apply_font_and_theme();
        dialog.layout();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &ExpandCDialogState {
        &self.state
    }

    pub unsafe fn set_loading(&mut self) {
        self.state = ExpandCDialogState {
            loading: true,
            message: crate::tr!("正在分析 C 盘可扩容空间..."),
            ..Default::default()
        };
        self.render_state();
    }

    pub unsafe fn apply_analysis(&mut self, analysis: ExpandCAnalysis) {
        self.state.apply_analysis(analysis);
        self.render_state();
    }

    pub unsafe fn set_executing(&mut self, executing: bool, message: impl Into<String>) {
        self.state.executing = executing;
        self.state.message = message.into();
        self.render_state();
    }

    pub unsafe fn set_error(&mut self, message: impl Into<String>) {
        self.state.loading = false;
        self.state.executing = false;
        self.state.message = message.into();
        self.render_state();
    }

    pub fn owns_target_edit(&self, control: HWND) -> bool {
        control == self.controls.target_edit
    }

    pub fn owns_slider(&self, control: HWND) -> bool {
        control == self.controls.slider
    }

    /// Called by the host for `EN_CHANGE` from the target-size editor.
    pub unsafe fn handle_target_edit_changed(&mut self) {
        let previous_movement = self.requires_partition_move();
        self.state.target_size_text = read_text(self.controls.target_edit);
        if let Ok(gb) = self.state.target_size_text.trim().parse::<f64>() {
            if gb.is_finite() && gb > 0.0 {
                self.state.target_size_mb = (gb * 1024.0).round() as u64;
                set_slider_position(self.controls.slider, self.state.target_size_mb);
            }
        }
        self.render_dynamic_state();
        if previous_movement != self.requires_partition_move() {
            self.layout();
            self.redraw_complete();
        }
    }

    /// Called by the host for `WM_HSCROLL` from the target-size trackbar.
    pub unsafe fn handle_slider_changed(&mut self) {
        let previous_movement = self.requires_partition_move();
        let position_tenths = SendMessageW(self.controls.slider, TBM_GETPOS, WPARAM(0), LPARAM(0))
            .0
            .clamp(i32::MIN as isize, i32::MAX as isize) as i32;
        self.state.apply_slider_position(position_tenths);
        set_text(self.controls.target_edit, &self.state.target_size_text);
        self.render_dynamic_state();
        if previous_movement != self.requires_partition_move() {
            self.layout();
            self.redraw_complete();
        }
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
        // The shell prepares every late-created child before the first visible frame. Re-assert
        // the two specialized roles afterwards, because the slider must keep its deterministic
        // Inno rendering and the size editor must keep the v6 CFD field theme.
        self.apply_font_and_theme();
        self.redraw_complete();
    }

    pub unsafe fn take_intent(&mut self) -> Option<ExpandCDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Secondary => {
                self.set_loading();
                Some(ExpandCDialogIntent::Analyze)
            }
            DialogResult::Primary => {
                self.state.target_size_text = read_text(self.controls.target_edit);
                match self.state.request() {
                    Ok(request) => Some(ExpandCDialogIntent::RequestConfirmation(request)),
                    Err(error) => {
                        self.state.message = error.to_string();
                        self.render_state();
                        None
                    }
                }
            }
            DialogResult::Cancel => Some(ExpandCDialogIntent::Close),
        }
    }

    pub unsafe fn layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_width = [
            self.controls.current_label,
            self.controls.used_label,
            self.controls.free_label,
            self.controls.max_label,
            self.controls.target_label,
        ]
        .iter()
        .map(|control| measure_text(self.shell.hwnd(), self.font, &read_text(*control), None).width)
        .max()
        .unwrap_or(0)
        .min(width / 2);
        let value_x = label_width + metrics.control_gap;
        let value_width = (width - value_x).max(0);
        let row_height = metrics.label_height;
        let mut y = 0;
        for (label, value) in [
            (self.controls.current_label, self.controls.current_value),
            (self.controls.used_label, self.controls.used_value),
            (self.controls.free_label, self.controls.free_value),
            (self.controls.max_label, self.controls.max_value),
        ] {
            let _ = MoveWindow(label, 0, y, label_width, row_height, true);
            let _ = MoveWindow(value, value_x, y, value_width, row_height, true);
            y += row_height + metrics.tight_gap;
        }
        y += metrics.control_gap;
        let _ = MoveWindow(
            self.controls.target_label,
            0,
            y + ((metrics.field_height - row_height) / 2).max(0),
            label_width,
            row_height,
            true,
        );
        let unit_width = measure_text(self.shell.hwnd(), self.font, "GB", None).width;
        let edit_width = scale(120, dpi).min(
            value_width
                .saturating_sub(unit_width)
                .saturating_sub(metrics.control_gap),
        );
        let _ = MoveWindow(
            self.controls.target_edit,
            value_x,
            y,
            edit_width,
            metrics.field_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.target_unit,
            value_x + edit_width + metrics.control_gap,
            y + ((metrics.field_height - row_height) / 2).max(0),
            unit_width,
            row_height,
            true,
        );
        y += metrics.field_height + metrics.control_gap;
        let slider_height = scale(24, dpi);
        let _ = MoveWindow(self.controls.slider, 0, y, width, slider_height, true);
        y += slider_height + metrics.tight_gap;
        let guidance_visible = self.state.analysis.found && self.state.analysis.can_expand;
        let range_height = if guidance_visible {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &read_text(self.controls.range),
                Some(width),
            )
            .height
            .max(row_height)
        } else {
            0
        };
        if range_height > 0 {
            let _ = MoveWindow(self.controls.range, 0, y, width, range_height, true);
            y += range_height + metrics.section_gap;
        }
        let _ = ShowWindow(
            self.controls.range,
            if range_height > 0 { SW_SHOW } else { SW_HIDE },
        );
        let movement = self
            .state
            .request()
            .as_ref()
            .is_ok_and(|request| request.requires_partition_move);
        let safety_height = if guidance_visible {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &read_text(self.controls.safety_note),
                Some(width),
            )
            .height
            .max(row_height)
        } else {
            0
        };
        let warning_height = if movement {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &read_text(self.controls.move_warning),
                Some(width),
            )
            .height
            .max(row_height)
        } else {
            0
        };
        let status_text = read_text(self.controls.status);
        let status_height = if status_text.is_empty() {
            0
        } else {
            measure_text(self.shell.hwnd(), self.font, &status_text, Some(width))
                .height
                .max(row_height)
        };
        let conditional = ExpandConditionalLayout::calculate_measured(
            y,
            metrics.control_gap,
            safety_height,
            warning_height,
            status_height,
        );
        self.shell
            .fit_content_height(logical_height(conditional.content_height, dpi));
        let _ = MoveWindow(
            self.controls.safety_note,
            0,
            conditional.safety_y,
            width,
            conditional.safety_height,
            true,
        );
        let _ = ShowWindow(
            self.controls.safety_note,
            if conditional.safety_height > 0 {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        if let Some(warning_y) = conditional.warning_y {
            let _ = MoveWindow(
                self.controls.move_warning,
                0,
                warning_y,
                width,
                conditional.warning_height,
                true,
            );
        }
        let _ = MoveWindow(
            self.controls.status,
            0,
            conditional.status_y,
            width,
            status_height,
            true,
        );
        let _ = ShowWindow(
            self.controls.status,
            if status_height > 0 { SW_SHOW } else { SW_HIDE },
        );
    }

    unsafe fn render_state(&mut self) {
        let analysis = &self.state.analysis;
        set_text(
            self.controls.current_value,
            &format_gb(analysis.current_size_mb),
        );
        set_text(self.controls.used_value, &format_gb(analysis.used_mb));
        set_text(self.controls.free_value, &format_gb(analysis.free_mb));
        set_text(self.controls.max_value, &format_gb(analysis.max_size_mb));
        set_text(self.controls.target_edit, &self.state.target_size_text);
        set_text(
            self.controls.range,
            &crate::tr!(
                "可设置范围: {} GB - {} GB",
                format_gb_value(self.state.min_target_mb()),
                format_gb_value(analysis.max_size_mb)
            ),
        );
        set_slider_range(
            self.controls.slider,
            self.state.min_target_mb(),
            analysis.max_size_mb,
        );
        set_slider_position(self.controls.slider, self.state.target_size_mb);
        let enabled =
            !self.state.loading && !self.state.executing && analysis.found && analysis.can_expand;
        let _ = EnableWindow(self.controls.target_edit, enabled);
        let _ = EnableWindow(self.controls.slider, enabled);
        self.render_dynamic_state();
        self.layout();
        self.redraw_complete();
    }

    unsafe fn render_dynamic_state(&self) {
        let request = self.state.request();
        let movement = request
            .as_ref()
            .is_ok_and(|request| request.requires_partition_move);
        let warning = if movement {
            crate::tr!(
                "⚠ 超过 {} GB 的部分需要移动 C 盘后方分区的数据来腾挪空间：\n· 该过程会搬移后方分区(如 D:)的数据，耗时较长；\n· 进行中切勿断电/强制关机，否则可能损坏后方分区；\n若只想要稳妥的纯扩展，请把目标控制在 {} GB 以内。",
                format_gb_value(self.state.analysis.no_move_max_mb),
                format_gb_value(self.state.analysis.no_move_max_mb)
            )
        } else {
            String::new()
        };
        if read_text(self.controls.move_warning) != warning {
            set_text(self.controls.move_warning, &warning);
        }
        let currently_visible = IsWindowVisible(self.controls.move_warning).as_bool();
        if currently_visible != movement {
            let _ = ShowWindow(
                self.controls.move_warning,
                if movement { SW_SHOW } else { SW_HIDE },
            );
        }
        let status = if !self.state.message.is_empty() {
            self.state.message.clone()
        } else if !self.state.analysis.reason.is_empty() {
            self.state.analysis.reason.clone()
        } else {
            String::new()
        };
        set_text(self.controls.status, &status);
        self.shell.set_primary_enabled(request.is_ok());
    }

    fn requires_partition_move(&self) -> bool {
        self.state
            .request()
            .as_ref()
            .is_ok_and(|request| request.requires_partition_move)
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        apply_control_theme(self.controls.target_edit, palette, NativeControlKind::Field);
        apply_trackbar_theme(self.controls.slider, palette);
    }

    unsafe fn redraw_complete(&self) {
        if !IsWindowVisible(self.shell.hwnd()).as_bool() {
            return;
        }
        // State transitions can resize the shell and move several transparent STATIC children in
        // one pass. Use the shell's same complete repaint path as the first visible frame. Keeping
        // RDW_NOERASE here preserves USER32's initial white backing surfaces after a visible
        // resize, which is especially obvious on the status STATIC and owner-draw command buttons.
        let _ = RedrawWindow(
            self.shell.hwnd(),
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
    }

    fn controls(&self) -> [HWND; 16] {
        let c = self.controls;
        [
            c.current_label,
            c.current_value,
            c.used_label,
            c.used_value,
            c.free_label,
            c.free_value,
            c.max_label,
            c.max_value,
            c.target_label,
            c.target_edit,
            c.target_unit,
            c.slider,
            c.range,
            c.safety_note,
            c.move_warning,
            c.status,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExpandConditionalLayout {
    safety_y: i32,
    safety_height: i32,
    warning_y: Option<i32>,
    warning_height: i32,
    status_y: i32,
    content_height: i32,
}

impl ExpandConditionalLayout {
    fn calculate(start_y: i32, scale_value: impl Fn(i32) -> i32, warning_visible: bool) -> Self {
        let safety_height = scale_value(36);
        let gap = scale_value(6);
        let warning_height = scale_value(70);
        let warning_y = warning_visible.then_some(start_y + safety_height + gap);
        let status_y = warning_y.map_or(start_y + safety_height + gap, |warning_y| {
            warning_y + warning_height + gap
        });
        Self {
            safety_y: start_y,
            safety_height,
            warning_y,
            warning_height,
            status_y,
            content_height: status_y + scale_value(22),
        }
    }

    fn calculate_measured(
        start_y: i32,
        gap: i32,
        safety_height: i32,
        warning_height: i32,
        status_height: i32,
    ) -> Self {
        let safety_height = safety_height.max(0);
        let warning_height = warning_height.max(0);
        let status_height = status_height.max(0);
        let mut cursor = start_y;
        if safety_height > 0 {
            cursor += safety_height;
        }
        let warning_y = if warning_height > 0 {
            if cursor > start_y {
                cursor += gap;
            }
            let y = cursor;
            cursor += warning_height;
            Some(y)
        } else {
            None
        };
        if status_height > 0 && cursor > start_y {
            cursor += gap;
        }
        let status_y = cursor;
        cursor += status_height;
        Self {
            safety_y: start_y,
            safety_height,
            warning_y,
            warning_height,
            status_y,
            content_height: cursor,
        }
    }
}

impl Drop for NativeExpandCDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<ExpandCControls> {
    let label = |text: &str, id: u16| child(parent, w!("STATIC"), text, 0, id);
    Ok(ExpandCControls {
        current_label: label(&crate::tr!("当前总大小:"), 64_610)?,
        current_value: label("", ID_CURRENT_SIZE)?,
        used_label: label(&crate::tr!("已用空间:"), 64_611)?,
        used_value: label("", ID_USED_SIZE)?,
        free_label: label(&crate::tr!("空闲空间:"), 64_612)?,
        free_value: label("", ID_FREE_SIZE)?,
        max_label: label(&crate::tr!("最大可扩容到:"), 64_613)?,
        max_value: label("", ID_MAX_SIZE)?,
        target_label: label(&crate::tr!("目标大小 (GB):"), 64_614)?,
        target_edit: child(
            parent,
            w!("EDIT"),
            "",
            WS_TABSTOP.0 as i32 | ES_AUTOHSCROLL,
            ID_TARGET_SIZE,
        )?,
        target_unit: label("GB", 64_615)?,
        slider: child(
            parent,
            w!("msctls_trackbar32"),
            "",
            WS_TABSTOP.0 as i32,
            ID_TARGET_SLIDER,
        )?,
        range: label("", ID_RANGE)?,
        safety_note: label(
            &crate::tr!(
                "提示: 此操作为无损扩容，C 盘数据会保留。\n若本机没有 WinPE，将先自动下载 WinPE；随后会安装 PE 引导并重启进入 WinPE 完成扩容。"
            ),
            64_616,
        )?,
        move_warning: label("", ID_MOVE_WARNING)?,
        status: label("", ID_STATUS)?,
    })
}

unsafe fn set_slider_range(slider: HWND, minimum_mb: u64, maximum_mb: u64) {
    let minimum = tenth_gb(minimum_mb);
    let maximum = tenth_gb(maximum_mb).max(minimum);
    let _ = SendMessageW(slider, TBM_SETRANGEMIN, WPARAM(0), LPARAM(minimum as isize));
    let _ = SendMessageW(slider, TBM_SETRANGEMAX, WPARAM(1), LPARAM(maximum as isize));
}

unsafe fn set_slider_position(slider: HWND, value_mb: u64) {
    let _ = SendMessageW(
        slider,
        TBM_SETPOS,
        WPARAM(1),
        LPARAM(tenth_gb(value_mb) as isize),
    );
}

fn tenth_gb(value_mb: u64) -> i32 {
    value_mb
        .saturating_mul(10)
        .saturating_add(512)
        .checked_div(1024)
        .unwrap_or(0)
        .min(i32::MAX as u64) as i32
}

fn mb_from_tenth_gb(position_tenths: i32) -> u64 {
    u64::try_from(position_tenths)
        .unwrap_or(0)
        .saturating_mul(1024)
        .saturating_add(5)
        / 10
}

fn format_gb(value_mb: u64) -> String {
    format!("{} GB", format_gb_value(value_mb))
}

fn format_gb_value(value_mb: u64) -> String {
    format!("{:.1}", value_mb as f64 / 1024.0)
}

unsafe fn read_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> ExpandCDialogState {
        let mut state = ExpandCDialogState::default();
        state.apply_analysis(ExpandCAnalysis {
            found: true,
            current_size_mb: 100 * 1024,
            used_mb: 70 * 1024,
            free_mb: 30 * 1024,
            no_move_max_mb: 120 * 1024,
            max_size_mb: 160 * 1024,
            can_expand: true,
            reason: String::new(),
        });
        state
    }

    #[test]
    fn legacy_default_selects_the_maximum() {
        let state = available();
        let request = state.request().unwrap();
        assert_eq!(request.target_size_mb, 160 * 1024);
        assert!(request.use_maximum);
        assert!(request.requires_partition_move);
    }

    #[test]
    fn adjacent_space_does_not_claim_a_partition_move() {
        let mut state = available();
        state.target_size_text = "115.0".into();
        let request = state.request().unwrap();
        assert!(!request.use_maximum);
        assert!(!request.requires_partition_move);
    }

    #[test]
    fn target_outside_the_analyzed_range_fails_closed() {
        let mut state = available();
        state.target_size_text = "200.0".into();
        assert!(matches!(
            state.request(),
            Err(ExpandCValidationError::OutsideRange { .. })
        ));
    }

    #[test]
    fn request_retains_analysis_snapshot_for_fresh_revalidation() {
        let state = available();
        let request = state.request().unwrap();
        assert_eq!(request.analyzed_current_size_mb, 100 * 1024);
        assert_eq!(request.analyzed_no_move_max_mb, 120 * 1024);
        assert_eq!(request.analyzed_max_size_mb, 160 * 1024);
    }

    #[test]
    fn hidden_move_warning_does_not_reserve_its_height() {
        let compact = ExpandConditionalLayout::calculate(200, |value| value, false);
        let warned = ExpandConditionalLayout::calculate(200, |value| value, true);
        assert_eq!(compact.warning_y, None);
        assert_eq!(compact.status_y, 242);
        assert_eq!(warned.warning_y, Some(242));
        assert_eq!(warned.status_y, 318);
        assert_eq!(warned.status_y - compact.status_y, 76);
    }

    #[test]
    fn conditional_layout_scales_every_vertical_metric() {
        let high_dpi = ExpandConditionalLayout::calculate(400, |value| value * 2, true);
        assert_eq!(high_dpi.safety_height, 72);
        assert_eq!(high_dpi.warning_y, Some(484));
        assert_eq!(high_dpi.status_y, 636);
    }

    #[test]
    fn measured_layout_does_not_leave_gaps_for_hidden_blocks() {
        let status_only = ExpandConditionalLayout::calculate_measured(200, 6, 0, 0, 22);
        assert_eq!(status_only.safety_height, 0);
        assert_eq!(status_only.warning_y, None);
        assert_eq!(status_only.status_y, 200);
        assert_eq!(status_only.content_height, 222);

        let completely_empty = ExpandConditionalLayout::calculate_measured(200, 6, 0, 0, 0);
        assert_eq!(completely_empty.content_height, 200);
    }

    #[test]
    fn slider_position_is_absolute_and_clamped_to_the_analyzed_range() {
        let mut state = available();
        state.apply_slider_position(1_150);
        assert_eq!(state.target_size_text, "115.0");
        assert_eq!(state.target_size_mb, 115 * 1024);

        // A spurious/default zero notification must not collapse a 100 GB minimum to zero.
        state.apply_slider_position(0);
        assert_eq!(state.target_size_text, "100.0");
        assert_eq!(state.target_size_mb, 100 * 1024);

        state.apply_slider_position(99_999);
        assert_eq!(state.target_size_text, "160.0");
        assert_eq!(state.target_size_mb, 160 * 1024);
    }

    #[test]
    fn tenth_gb_mapping_is_monotonic_and_round_trips_every_slider_step() {
        let mut previous_mb = 0;
        for position in 1_000..=1_600 {
            let megabytes = mb_from_tenth_gb(position);
            assert!(megabytes >= previous_mb);
            assert_eq!(tenth_gb(megabytes), position);
            previous_mb = megabytes;
        }
    }
}
