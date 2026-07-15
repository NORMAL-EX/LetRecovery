//! Native, side-effect-free dialogs for the first read-only toolbox migration.
//!
//! The controls collect input and display snapshots only.  Every operation is
//! returned as an intent so the window/controller can dispatch the existing
//! asynchronous business implementation outside a window procedure.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, ScreenToClient, HFONT};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMTEXTW, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetDlgItem, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, MoveWindow,
    SendMessageW, SetWindowTextW, ShowWindow, BS_OWNERDRAW, BS_PUSHBUTTON, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, SW_HIDE, SW_SHOW, WM_SETFONT, WS_BORDER, WS_TABSTOP,
    WS_VSCROLL,
};

use super::controls::{child, wide};
use super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::layout::{
    arrange_field, measure_text, measured_button_width, preferred_list_height, FieldArrangement,
    LayoutMetrics,
};
use super::theme::{apply_control_theme, apply_list_view_theme, NativeControlKind, Palette};

const ID_FIRST_TOOL_DIALOG_CONTROL: u16 = 63_100;
const ID_TOOL_DIALOG_ACTION: u16 = ID_FIRST_TOOL_DIALOG_CONTROL + 5;
const ID_TOOL_DIALOG_BROWSE: u16 = ID_FIRST_TOOL_DIALOG_CONTROL + 6;
const ID_STANDARD_DIALOG_PRIMARY: i32 = 61_002;
const ID_STANDARD_DIALOG_CANCEL: i32 = 61_004;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolDialogKind {
    NetworkInformation,
    SoftwareList,
    ReadGhoPassword,
    VerifyImage,
    VerifyFileHash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolDialogIntent {
    RefreshNetworkInformation,
    CopyNetworkReport,
    RefreshSoftwareList,
    ExportSoftwareList,
    BrowseGhoImage,
    ReadGhoPassword { path: String },
    CopyGhoPassword { password: String },
    BrowseImageForVerification,
    VerifyImage { path: String },
    CancelImageVerification,
    BrowseFileForHash,
    VerifyFileHash { path: String, expected: String },
    Close,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkInformationState {
    pub loading: bool,
    pub report: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoftwareListState {
    pub loading: bool,
    pub records: Vec<crate::core::native_tool_executor::InstalledSoftwareRecord>,
    pub rows: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GhoPasswordState {
    pub path: String,
    pub reading: bool,
    pub outcome: Option<crate::core::native_tool_executor::GhoPasswordResult>,
    pub result: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageVerificationState {
    pub path: String,
    pub verifying: bool,
    pub percentage: u8,
    pub outcome: Option<crate::core::native_tool_executor::ImageVerificationResult>,
    pub result: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileHashState {
    pub path: String,
    pub expected: String,
    pub verifying: bool,
    pub percentage: u8,
    pub outcome: Option<crate::core::native_tool_executor::Sha256Result>,
    pub result: String,
}

#[derive(Default)]
struct ToolControls {
    first_label: HWND,
    first_edit: HWND,
    second_label: HWND,
    second_edit: HWND,
    report: HWND,
    browse_button: HWND,
    action_button: HWND,
}

/// Owns standard dialog chrome and the compact content controls for one tool.
pub struct NativeToolDialog {
    kind: ToolDialogKind,
    pub shell: DialogShell,
    controls: ToolControls,
    font: HFONT,
    report_cache: RefCell<String>,
    gho_password: RefCell<Option<String>>,
    image_verifying: Cell<bool>,
    visible_row_count: Cell<usize>,
    action_visible: Cell<bool>,
}

impl NativeToolDialog {
    pub unsafe fn create(owner: HWND, kind: ToolDialogKind) -> windows::core::Result<Self> {
        let shell = DialogShell::create(owner, dialog_spec(kind))?;
        let content = shell.content();
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
        let controls = create_controls(content, kind)?;
        let mut dialog = Self {
            kind,
            shell,
            controls,
            font,
            report_cache: RefCell::new(String::new()),
            gho_password: RefCell::new(None),
            image_verifying: Cell::new(false),
            visible_row_count: Cell::new(0),
            action_visible: Cell::new(false),
        };
        dialog.apply_font_and_theme();
        dialog.set_action_visible(false);
        dialog.refit_and_layout();
        dialog
            .shell
            .set_primary_enabled(primary_is_initially_enabled(kind));
        Ok(dialog)
    }

    pub const fn kind(&self) -> ToolDialogKind {
        self.kind
    }

    pub fn owns_content_action(&self, control: HWND) -> bool {
        (!self.controls.action_button.is_invalid() && control == self.controls.action_button)
            || (!self.controls.browse_button.is_invalid() && control == self.controls.browse_button)
    }

    /// Host hook for the dedicated GHO-copy or image-cancel content button.
    pub fn handle_content_action(&self, command_id: u16) -> Option<ToolDialogIntent> {
        if command_id == ID_TOOL_DIALOG_BROWSE {
            return browse_intent(self.kind);
        }
        if command_id != ID_TOOL_DIALOG_ACTION {
            return None;
        }
        match self.kind {
            ToolDialogKind::ReadGhoPassword => self
                .gho_password
                .borrow()
                .clone()
                .map(|password| ToolDialogIntent::CopyGhoPassword { password }),
            ToolDialogKind::VerifyImage if self.image_verifying.get() => {
                Some(ToolDialogIntent::CancelImageVerification)
            }
            _ => None,
        }
    }

    pub unsafe fn report_text(&self) -> String {
        let cached = self.report_cache.borrow();
        if cached.is_empty() {
            get_text(self.controls.report)
        } else {
            cached.clone()
        }
    }

    /// Recalculates content at the shell's current DPI before showing it.
    pub unsafe fn show_modeless(&mut self) {
        self.refit_and_layout();
        self.shell.show_modeless();
        // The shell's pre-show pass intentionally uses a conservative generic role for every
        // Edit.  Restore the tool-specific role after that pass: single-line path fields use
        // CFD/DarkMode_CFD, while reports use Explorer/DarkMode_Explorer so their client area
        // and native scrollbar follow the same light/dark palette.
        self.apply_font_and_theme();
        // `DialogShell::show_modeless` performs a final descendant/theme pass immediately before
        // showing the window. Restore the dedicated content-row Browse geometry after that pass.
        self.layout();
    }

    pub unsafe fn take_intent(&mut self) -> Option<ToolDialogIntent> {
        self.shell
            .take_result()
            .map(|result| self.intent_for_result(result))
    }

    /// Maps standard command-bar results without performing their operation.
    pub unsafe fn intent_for_result(&self, result: DialogResult) -> ToolDialogIntent {
        match (self.kind, result) {
            (_, DialogResult::Cancel) => ToolDialogIntent::Close,
            (ToolDialogKind::NetworkInformation, DialogResult::Primary) => {
                ToolDialogIntent::CopyNetworkReport
            }
            (ToolDialogKind::NetworkInformation, DialogResult::Secondary) => {
                ToolDialogIntent::RefreshNetworkInformation
            }
            (ToolDialogKind::SoftwareList, DialogResult::Primary) => {
                ToolDialogIntent::ExportSoftwareList
            }
            (ToolDialogKind::SoftwareList, DialogResult::Secondary) => {
                ToolDialogIntent::RefreshSoftwareList
            }
            (ToolDialogKind::ReadGhoPassword, DialogResult::Primary) => {
                ToolDialogIntent::ReadGhoPassword {
                    path: get_text(self.controls.first_edit),
                }
            }
            (ToolDialogKind::ReadGhoPassword, DialogResult::Secondary) => ToolDialogIntent::Close,
            (ToolDialogKind::VerifyImage, DialogResult::Primary) => ToolDialogIntent::VerifyImage {
                path: get_text(self.controls.first_edit),
            },
            (ToolDialogKind::VerifyImage, DialogResult::Secondary) => ToolDialogIntent::Close,
            (ToolDialogKind::VerifyFileHash, DialogResult::Primary) => {
                ToolDialogIntent::VerifyFileHash {
                    path: get_text(self.controls.first_edit),
                    expected: get_text(self.controls.second_edit),
                }
            }
            (ToolDialogKind::VerifyFileHash, DialogResult::Secondary) => ToolDialogIntent::Close,
        }
    }

    pub unsafe fn set_network_state(&mut self, state: &NetworkInformationState) {
        if self.kind != ToolDialogKind::NetworkInformation {
            return;
        }
        let report = if state.loading {
            crate::tr!("正在读取网络适配器信息…")
        } else {
            state.report.clone()
        };
        *self.report_cache.borrow_mut() = state.report.clone();
        set_text(self.controls.report, &report);
        self.visible_row_count.set(report_line_count(&report));
        self.shell
            .set_primary_enabled(!state.loading && !state.report.is_empty());
        self.refit_and_layout();
    }

    pub unsafe fn set_software_state(&mut self, state: &SoftwareListState) {
        if self.kind != ToolDialogKind::SoftwareList {
            return;
        }
        let count = if state.records.is_empty() {
            state.rows.len()
        } else {
            state.records.len()
        };
        set_text(
            self.controls.first_label,
            &if state.loading {
                crate::tr!("正在读取已安装软件…")
            } else {
                crate::tr!("共 {} 个软件", count)
            },
        );
        refill_software_list(self.controls.report, state);
        self.visible_row_count.set(count);
        *self.report_cache.borrow_mut() = software_export_text(state);
        self.shell.set_primary_enabled(
            !state.loading && (!state.records.is_empty() || !state.rows.is_empty()),
        );
        self.refit_and_layout();
    }

    pub unsafe fn set_gho_password_state(&mut self, state: &GhoPasswordState) {
        if self.kind != ToolDialogKind::ReadGhoPassword {
            return;
        }
        set_text(self.controls.first_edit, &state.path);
        let report = if state.reading {
            crate::tr!("正在读取 GHO 文件头…")
        } else if let Some(outcome) = &state.outcome {
            format_gho_result(outcome)
        } else {
            state.result.clone()
        };
        *self.gho_password.borrow_mut() = state.outcome.as_ref().and_then(copyable_gho_password);
        let _ = EnableWindow(
            self.controls.action_button,
            !state.reading && self.gho_password.borrow().is_some(),
        );
        *self.report_cache.borrow_mut() = report.clone();
        set_text(self.controls.report, &report);
        self.visible_row_count.set(report_line_count(&report));
        let has_password = self.gho_password.borrow().is_some();
        self.set_action_visible(has_password);
        self.shell
            .set_primary_enabled(!state.reading && !state.path.trim().is_empty());
        self.refit_and_layout();
    }

    pub unsafe fn set_image_verification_state(&mut self, state: &ImageVerificationState) {
        if self.kind != ToolDialogKind::VerifyImage {
            return;
        }
        set_text(self.controls.first_edit, &state.path);
        let report = if state.verifying {
            progress_report(true, state.percentage, &state.result)
        } else if let Some(outcome) = &state.outcome {
            format_image_result(outcome)
        } else {
            state.result.clone()
        };
        let row_count = report_line_count(&report);
        set_text(self.controls.report, &report);
        *self.report_cache.borrow_mut() = report;
        self.image_verifying.set(state.verifying);
        self.visible_row_count.set(row_count);
        self.set_action_visible(state.verifying);
        let _ = EnableWindow(self.controls.action_button, state.verifying);
        self.shell
            .set_primary_enabled(!state.verifying && !state.path.trim().is_empty());
        self.refit_and_layout();
    }

    pub unsafe fn set_file_hash_state(&mut self, state: &FileHashState) {
        if self.kind != ToolDialogKind::VerifyFileHash {
            return;
        }
        set_text(self.controls.first_edit, &state.path);
        set_text(self.controls.second_edit, &state.expected);
        let report = if state.verifying {
            progress_report(true, state.percentage, &state.result)
        } else if let Some(outcome) = &state.outcome {
            format_hash_result(outcome)
        } else {
            state.result.clone()
        };
        set_text(self.controls.report, &report);
        self.visible_row_count.set(report_line_count(&report));
        *self.report_cache.borrow_mut() = report;
        self.shell
            .set_primary_enabled(!state.verifying && !state.path.trim().is_empty());
        self.refit_and_layout();
    }

    unsafe fn apply_font_and_theme(&self) {
        for control in self.all_controls() {
            if !control.is_invalid() {
                let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
            }
        }
        let palette = Palette::system();
        for control in [self.controls.first_label, self.controls.second_label] {
            apply_valid_theme(control, palette, NativeControlKind::General);
        }
        for control in [self.controls.first_edit, self.controls.second_edit] {
            apply_valid_theme(control, palette, NativeControlKind::Field);
        }
        apply_valid_theme(
            self.controls.action_button,
            palette,
            NativeControlKind::General,
        );
        apply_valid_theme(
            self.controls.browse_button,
            palette,
            NativeControlKind::General,
        );
        if self.kind == ToolDialogKind::SoftwareList {
            let _ = apply_list_view_theme(self.controls.report, palette);
            for (message, color) in [
                (LVM_SETBKCOLOR, palette.edit),
                (LVM_SETTEXTBKCOLOR, palette.edit),
                (LVM_SETTEXTCOLOR, palette.text),
            ] {
                let _ = SendMessageW(
                    self.controls.report,
                    message,
                    WPARAM(0),
                    LPARAM(color.0 as isize),
                );
            }
        } else if let Some(kind) = report_control_kind(self.kind) {
            apply_valid_theme(self.controls.report, palette, kind);
        }
    }

    unsafe fn set_action_visible(&mut self, visible: bool) {
        let visible = visible && !self.controls.action_button.is_invalid();
        self.action_visible.set(visible);
        if !self.controls.action_button.is_invalid() {
            let _ = ShowWindow(
                self.controls.action_button,
                if visible { SW_SHOW } else { SW_HIDE },
            );
        }
    }

    /// Fits the outer shell to the controls that are actually visible.  The shell API accepts a
    /// 96-DPI logical height, while all geometry below is measured in the current monitor's
    /// physical pixels.
    unsafe fn refit_and_layout(&mut self) {
        let content = self.shell.content();
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut client = RECT::default();
        let _ = GetClientRect(content, &mut client);
        let width = (client.right - client.left).max(0);
        let preferred = self.preferred_content_height(width, dpi);
        self.shell.fit_content_height(unscale(preferred, dpi));
        self.layout();
    }

    unsafe fn preferred_content_height(&self, width: i32, dpi: u32) -> i32 {
        let metrics = LayoutMetrics::for_dpi(dpi);
        let report_height = preferred_list_height(self.visible_row_count.get(), dpi, 3, 8);
        match self.kind {
            ToolDialogKind::NetworkInformation => report_height,
            ToolDialogKind::SoftwareList => {
                let label_height = self.measured_label_height(self.controls.first_label, dpi);
                label_height + metrics.control_gap + report_height
            }
            ToolDialogKind::ReadGhoPassword | ToolDialogKind::VerifyImage => {
                let field = self.field_geometry(self.controls.first_label, 0, width, dpi, true);
                let action_height = if self.action_visible.get() {
                    metrics.control_gap + metrics.button_height
                } else {
                    0
                };
                field.bottom + action_height + metrics.section_gap + report_height
            }
            ToolDialogKind::VerifyFileHash => {
                let first = self.field_geometry(self.controls.first_label, 0, width, dpi, true);
                let second = self.field_geometry(
                    self.controls.second_label,
                    first.bottom + metrics.control_gap,
                    width,
                    dpi,
                    false,
                );
                second.bottom + metrics.section_gap + report_height
            }
        }
    }

    unsafe fn measured_label_height(&self, label: HWND, dpi: u32) -> i32 {
        let metrics = LayoutMetrics::for_dpi(dpi);
        measure_text(self.shell.hwnd(), self.font, &get_text(label), None)
            .height
            .max(metrics.label_height)
    }

    unsafe fn field_geometry(
        &self,
        label: HWND,
        y: i32,
        width: i32,
        dpi: u32,
        has_browse: bool,
    ) -> FieldGeometry {
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_size = measure_text(self.shell.hwnd(), self.font, &get_text(label), None);
        let label_height = label_size.height.max(metrics.label_height);
        let browse = if has_browse {
            (!self.controls.browse_button.is_invalid()).then(|| {
                measured_button_width(
                    self.shell.hwnd(),
                    self.font,
                    &get_text(self.controls.browse_button),
                    dpi,
                    scale(75, dpi),
                )
            })
        } else {
            None
        };
        let browse_and_gap = browse
            .map(|button_width| button_width + metrics.control_gap)
            .unwrap_or(0);
        let minimum_field_width = scale(220, dpi);
        let minimum_control_width = minimum_field_width + browse_and_gap;
        let field_row_height = metrics.field_height.max(metrics.button_height);

        match arrange_field(width, label_size.width, minimum_control_width, dpi) {
            FieldArrangement::Inline {
                label_width,
                control_x,
                control_width,
            } => {
                let row_height = label_height.max(field_row_height);
                let field_y = y + (row_height - metrics.field_height) / 2;
                let label_y = y + (row_height - label_height) / 2;
                let browse_bounds = browse.map(|button_width| ControlBounds {
                    x: control_x + (control_width - button_width).max(0),
                    y: y + (row_height - metrics.button_height) / 2,
                    width: button_width.min(control_width.max(0)),
                    height: metrics.button_height,
                });
                let edit_width = browse_bounds
                    .map(|button| button.x - metrics.control_gap - control_x)
                    .unwrap_or(control_width)
                    .max(0);
                FieldGeometry {
                    label: ControlBounds {
                        x: 0,
                        y: label_y,
                        width: label_width,
                        height: label_height,
                    },
                    edit: ControlBounds {
                        x: control_x,
                        y: field_y,
                        width: edit_width,
                        height: metrics.field_height,
                    },
                    browse: browse_bounds,
                    bottom: y + row_height,
                }
            }
            FieldArrangement::Stacked => {
                let row_y = y + label_height + metrics.tight_gap;
                let browse_bounds = browse.map(|button_width| ControlBounds {
                    x: (width - button_width).max(0),
                    y: row_y + (field_row_height - metrics.button_height) / 2,
                    width: button_width.min(width.max(0)),
                    height: metrics.button_height,
                });
                let edit_width = browse_bounds
                    .map(|button| button.x - metrics.control_gap)
                    .unwrap_or(width)
                    .max(0);
                FieldGeometry {
                    label: ControlBounds {
                        x: 0,
                        y,
                        width,
                        height: label_height,
                    },
                    edit: ControlBounds {
                        x: 0,
                        y: row_y + (field_row_height - metrics.field_height) / 2,
                        width: edit_width,
                        height: metrics.field_height,
                    },
                    browse: browse_bounds,
                    bottom: row_y + field_row_height,
                }
            }
        }
    }

    unsafe fn layout(&self) {
        let content = self.shell.content();
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut client = RECT::default();
        let _ = GetClientRect(content, &mut client);
        let width = (client.right - client.left).max(0);
        let height = (client.bottom - client.top).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);

        match self.kind {
            ToolDialogKind::NetworkInformation => {
                move_control(self.controls.report, 0, 0, width, height);
            }
            ToolDialogKind::SoftwareList => {
                let label_height = self.measured_label_height(self.controls.first_label, dpi);
                move_control(self.controls.first_label, 0, 0, width, label_height);
                let list_y = label_height + metrics.control_gap;
                move_control(self.controls.report, 0, list_y, width, height - list_y);
                set_software_column_widths(self.controls.report, width);
                self.ensure_software_export_button_width(dpi);
            }
            ToolDialogKind::ReadGhoPassword | ToolDialogKind::VerifyImage => {
                let field = self.field_geometry(self.controls.first_label, 0, width, dpi, true);
                field.apply(
                    self.controls.first_label,
                    self.controls.first_edit,
                    |bounds| self.move_browse_button_next_to_edit(bounds),
                );
                let mut report_y = field.bottom;
                if self.action_visible.get() {
                    report_y += metrics.control_gap;
                    let action_width = measured_button_width(
                        self.shell.hwnd(),
                        self.font,
                        &get_text(self.controls.action_button),
                        dpi,
                        scale(75, dpi),
                    )
                    .min(width.max(0));
                    move_control(
                        self.controls.action_button,
                        (width - action_width).max(0),
                        report_y,
                        action_width,
                        metrics.button_height,
                    );
                    report_y += metrics.button_height;
                }
                report_y += metrics.section_gap;
                move_control(self.controls.report, 0, report_y, width, height - report_y);
            }
            ToolDialogKind::VerifyFileHash => {
                let first = self.field_geometry(self.controls.first_label, 0, width, dpi, true);
                first.apply(
                    self.controls.first_label,
                    self.controls.first_edit,
                    |bounds| self.move_browse_button_next_to_edit(bounds),
                );
                let second = self.field_geometry(
                    self.controls.second_label,
                    first.bottom + metrics.control_gap,
                    width,
                    dpi,
                    false,
                );
                second.apply(
                    self.controls.second_label,
                    self.controls.second_edit,
                    |_| {},
                );
                let report_y = second.bottom + metrics.section_gap;
                move_control(self.controls.report, 0, report_y, width, height - report_y);
            }
        }
    }

    /// Browse is a content-row command: unlike a standard shell result it must not hide the dialog
    /// before the host opens the file picker and refills the selected path.
    unsafe fn move_browse_button_next_to_edit(&self, bounds: ControlBounds) {
        let button = self.controls.browse_button;
        if button.is_invalid() {
            return;
        }
        let _ = MoveWindow(
            button,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            true,
        );
    }

    /// `DialogShell` already measures command captions. Keep an additional conservative minimum
    /// for the longest tool caption so Microsoft YaHei UI fallback metrics cannot ellipsize
    /// “保存列表为TXT” / “Save list as TXT” at 200% DPI. The existing command height and right edge
    /// remain unchanged, and an optional cancel button retains the standard ten-pixel gap.
    unsafe fn ensure_software_export_button_width(&self, dpi: u32) {
        let Ok(primary) = GetDlgItem(self.shell.hwnd(), ID_STANDARD_DIALOG_PRIMARY) else {
            return;
        };
        let Some(primary_rect) = child_rect_in_parent(primary, self.shell.hwnd()) else {
            return;
        };
        let cancel_left = GetDlgItem(self.shell.hwnd(), ID_STANDARD_DIALOG_CANCEL)
            .ok()
            .and_then(|cancel| child_rect_in_parent(cancel, self.shell.hwnd()))
            .map(|rect| rect.left);
        let current_width = (primary_rect.right - primary_rect.left).max(0);
        let (x, width) = software_export_button_geometry(
            self.shell_client_width(),
            dpi,
            current_width,
            cancel_left,
        );
        let _ = MoveWindow(
            primary,
            x,
            primary_rect.top,
            width,
            (primary_rect.bottom - primary_rect.top).max(0),
            true,
        );
    }

    unsafe fn shell_client_width(&self) -> i32 {
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.hwnd(), &mut rect);
        (rect.right - rect.left).max(0)
    }

    fn all_controls(&self) -> [HWND; 7] {
        [
            self.controls.first_label,
            self.controls.first_edit,
            self.controls.second_label,
            self.controls.second_edit,
            self.controls.report,
            self.controls.browse_button,
            self.controls.action_button,
        ]
    }
}

fn browse_intent(kind: ToolDialogKind) -> Option<ToolDialogIntent> {
    match kind {
        ToolDialogKind::ReadGhoPassword => Some(ToolDialogIntent::BrowseGhoImage),
        ToolDialogKind::VerifyImage => Some(ToolDialogIntent::BrowseImageForVerification),
        ToolDialogKind::VerifyFileHash => Some(ToolDialogIntent::BrowseFileForHash),
        ToolDialogKind::NetworkInformation | ToolDialogKind::SoftwareList => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ControlBounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FieldGeometry {
    label: ControlBounds,
    edit: ControlBounds,
    browse: Option<ControlBounds>,
    bottom: i32,
}

impl FieldGeometry {
    unsafe fn apply(self, label: HWND, edit: HWND, place_browse: impl FnOnce(ControlBounds)) {
        move_control(
            label,
            self.label.x,
            self.label.y,
            self.label.width,
            self.label.height,
        );
        move_control(
            edit,
            self.edit.x,
            self.edit.y,
            self.edit.width,
            self.edit.height,
        );
        if let Some(bounds) = self.browse {
            place_browse(bounds);
        }
    }
}

unsafe fn apply_valid_theme(control: HWND, palette: Palette, kind: NativeControlKind) {
    if !control.is_invalid() {
        apply_control_theme(control, palette, kind);
    }
}

const fn report_control_kind(kind: ToolDialogKind) -> Option<NativeControlKind> {
    match kind {
        ToolDialogKind::SoftwareList => None,
        ToolDialogKind::NetworkInformation
        | ToolDialogKind::ReadGhoPassword
        | ToolDialogKind::VerifyImage
        | ToolDialogKind::VerifyFileHash => Some(NativeControlKind::ScrollableField),
    }
}

impl Drop for NativeToolDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

fn dialog_spec(kind: ToolDialogKind) -> DialogSpec {
    match kind {
        ToolDialogKind::NetworkInformation => DialogSpec {
            window_title: crate::tr!("网络信息"),
            title: crate::tr!("网络信息"),
            description: crate::tr!("查看当前网络适配器、地址、网关和 DNS 信息。"),
            width: 700,
            height: 500,
            buttons: DialogButtons {
                primary: crate::tr!("复制"),
                secondary: None,
                cancel: Some(crate::tr!("关闭")),
            },
        },
        ToolDialogKind::SoftwareList => DialogSpec {
            window_title: crate::tr!("软件列表"),
            title: crate::tr!("已安装软件"),
            description: crate::tr!("查看并导出当前系统的软件清单。"),
            width: 760,
            height: 540,
            buttons: DialogButtons {
                primary: crate::tr!("保存列表为TXT"),
                secondary: None,
                cancel: Some(crate::tr!("关闭")),
            },
        },
        ToolDialogKind::ReadGhoPassword => DialogSpec {
            window_title: crate::tr!("查看 GHO 密码"),
            title: crate::tr!("查看 GHO 密码"),
            description: crate::tr!("只读取 Ghost 镜像文件头，不修改镜像。"),
            width: 650,
            height: 350,
            buttons: DialogButtons {
                primary: crate::tr!("读取"),
                secondary: None,
                cancel: Some(crate::tr!("关闭")),
            },
        },
        ToolDialogKind::VerifyImage => DialogSpec {
            window_title: crate::tr!("校验系统镜像"),
            title: crate::tr!("校验系统镜像"),
            description: crate::tr!("验证 WIM、ESD、SWM、GHO 或 ISO 镜像的完整性。"),
            width: 680,
            height: 400,
            buttons: DialogButtons {
                primary: crate::tr!("开始校验"),
                secondary: None,
                cancel: Some(crate::tr!("关闭")),
            },
        },
        ToolDialogKind::VerifyFileHash => DialogSpec {
            window_title: crate::tr!("文件哈希校验"),
            title: crate::tr!("文件哈希校验"),
            description: crate::tr!("计算 SHA-256；填写预期值后同时进行比对。"),
            width: 680,
            height: 420,
            buttons: DialogButtons {
                primary: crate::tr!("开始校验"),
                secondary: None,
                cancel: Some(crate::tr!("关闭")),
            },
        },
    }
}

unsafe fn create_controls(
    parent: HWND,
    kind: ToolDialogKind,
) -> windows::core::Result<ToolControls> {
    let mut controls = ToolControls::default();
    let report_style = ES_MULTILINE
        | ES_AUTOVSCROLL
        | ES_READONLY
        | WS_BORDER.0 as i32
        | WS_VSCROLL.0 as i32
        | WS_TABSTOP.0 as i32;
    // Single-line edits intentionally start borderless. `NativeControlKind::Field` applies the
    // Windows 11 property-page WS_EX_CLIENTEDGE + CFD/DarkMode_CFD combination before display.
    let edit_style = ES_AUTOHSCROLL | WS_TABSTOP.0 as i32;

    if !matches!(
        kind,
        ToolDialogKind::NetworkInformation | ToolDialogKind::SoftwareList
    ) {
        let label = match kind {
            ToolDialogKind::ReadGhoPassword => crate::tr!("GHO 镜像："),
            ToolDialogKind::VerifyImage => crate::tr!("系统镜像："),
            ToolDialogKind::VerifyFileHash => crate::tr!("文件："),
            ToolDialogKind::NetworkInformation | ToolDialogKind::SoftwareList => unreachable!(),
        };
        controls.first_label = child(parent, w!("STATIC"), &label, 0, control_id(0))?;
        controls.first_edit = child(parent, w!("EDIT"), "", edit_style, control_id(1))?;
        controls.browse_button = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("浏览…"),
            content_command_button_style(),
            ID_TOOL_DIALOG_BROWSE,
        )?;
    }
    if kind == ToolDialogKind::SoftwareList {
        controls.first_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("共 {} 个软件", 0),
            0,
            control_id(0),
        )?;
    }
    if kind == ToolDialogKind::VerifyFileHash {
        controls.second_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("预期 SHA-256（可选）："),
            0,
            control_id(2),
        )?;
        controls.second_edit = child(parent, w!("EDIT"), "", edit_style, control_id(3))?;
    }
    controls.report = if kind == ToolDialogKind::SoftwareList {
        let report = child(
            parent,
            w!("SysListView32"),
            "",
            (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
            control_id(4),
        )?;
        let _ = SendMessageW(
            report,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
        );
        initialize_software_columns(report);
        report
    } else {
        child(
            parent,
            w!("EDIT"),
            &initial_report(kind),
            report_style,
            control_id(4),
        )?
    };
    if matches!(
        kind,
        ToolDialogKind::ReadGhoPassword | ToolDialogKind::VerifyImage
    ) {
        let label = if kind == ToolDialogKind::ReadGhoPassword {
            crate::tr!("复制密码")
        } else {
            crate::tr!("取消校验")
        };
        controls.action_button = child(
            parent,
            w!("BUTTON"),
            &label,
            BS_PUSHBUTTON | WS_TABSTOP.0 as i32,
            ID_TOOL_DIALOG_ACTION,
        )?;
        let _ = EnableWindow(controls.action_button, false);
    }
    Ok(controls)
}

const fn control_id(offset: u16) -> u16 {
    ID_FIRST_TOOL_DIALOG_CONTROL + offset
}

fn initial_report(kind: ToolDialogKind) -> String {
    match kind {
        ToolDialogKind::NetworkInformation => crate::tr!("尚未读取网络信息。"),
        ToolDialogKind::SoftwareList => crate::tr!("尚未读取软件列表。"),
        ToolDialogKind::ReadGhoPassword => crate::tr!("请选择 GHO 镜像。"),
        ToolDialogKind::VerifyImage => crate::tr!("请选择要校验的系统镜像。"),
        ToolDialogKind::VerifyFileHash => crate::tr!("请选择要计算 SHA-256 的文件。"),
    }
}

const fn primary_is_initially_enabled(_kind: ToolDialogKind) -> bool {
    // Copy/export/read/verify stay disabled until the controller supplies a
    // loaded report or a non-empty path.
    false
}

fn progress_report(running: bool, percentage: u8, result: &str) -> String {
    if running {
        crate::tr!("正在校验… {}%", percentage.min(100))
    } else {
        result.to_string()
    }
}

fn software_export_text(state: &SoftwareListState) -> String {
    if state.loading {
        return String::new();
    }
    if state.records.is_empty() {
        return state.rows.join("\r\n");
    }
    let mut content = crate::tr!(
        "已安装软件列表 - 导出时间: {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    content.push_str(&"=".repeat(100));
    content.push('\n');
    content.push_str(&format!(
        "{:<50} {:<20} {:<30}\n",
        crate::tr!("软件名称"),
        crate::tr!("版本"),
        crate::tr!("发布者")
    ));
    content.push_str(&"-".repeat(100));
    content.push('\n');
    for record in &state.records {
        content.push_str(&format!(
            "{:<50} {:<20} {:<30}\n",
            truncate_text(&record.name, 48),
            truncate_text(&record.version, 18),
            truncate_text(&record.publisher, 28)
        ));
    }
    content.push_str(&"=".repeat(100));
    content.push('\n');
    content.push_str(&crate::tr!("共 {} 个软件\n", state.records.len()));
    content
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_owned()
    } else {
        let keep = max_chars.saturating_sub(3);
        format!("{}...", value.chars().take(keep).collect::<String>())
    }
}

fn copyable_gho_password(
    result: &crate::core::native_tool_executor::GhoPasswordResult,
) -> Option<String> {
    if result.valid && result.has_password {
        result
            .password
            .clone()
            .filter(|password| !password.is_empty())
    } else {
        None
    }
}

fn format_gho_result(result: &crate::core::native_tool_executor::GhoPasswordResult) -> String {
    if let Some(error) = &result.error {
        return crate::tr!("操作失败：{}", error);
    }
    if !result.valid {
        return crate::tr!("所选文件不是有效的 GHO 镜像。");
    }
    if !result.has_password {
        return crate::tr!("此 GHO 镜像未设置密码。");
    }
    match &result.password {
        Some(password) => crate::tr!(
            "镜像密码：{}\r\n密码长度：{}",
            password,
            result.password_length
        ),
        None => crate::tr!("镜像已设置密码，但当前文件头无法解出密码。"),
    }
}

fn format_image_result(
    result: &crate::core::native_tool_executor::ImageVerificationResult,
) -> String {
    let mut lines = vec![
        crate::tr!("文件：{}", result.path),
        crate::tr!("类型：{}", result.image_type),
        crate::tr!("大小：{}", format_file_size(result.file_size)),
        crate::tr!("状态：{}", result.status),
    ];
    if !result.message.is_empty() {
        lines.push(crate::tr!("说明：{}", result.message));
    }
    if result.image_count > 0 {
        lines.push(crate::tr!("镜像数量：{}", result.image_count));
    }
    if result.part_count > 1 {
        lines.push(crate::tr!("分卷数量：{}", result.part_count));
    }
    lines.extend(result.details.iter().cloned());
    lines.join("\r\n")
}

fn format_hash_result(result: &crate::core::native_tool_executor::Sha256Result) -> String {
    let mut lines = vec![
        crate::tr!("文件：{}", result.path),
        crate::tr!("大小：{}", format_file_size(result.file_size)),
        format!("SHA-256: {}", result.sha256),
    ];
    lines.push(match result.matched {
        Some(true) => crate::tr!("与期望哈希一致"),
        Some(false) => crate::tr!("与期望哈希不一致（文件可能损坏或被篡改）"),
        None => crate::tr!("（未提供期望哈希，仅展示计算值）"),
    });
    lines.join("\r\n")
}

fn format_file_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if size >= GB {
        format!("{:.2} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.2} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.2} KB", size as f64 / KB as f64)
    } else {
        crate::tr!("{} 字节", size)
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

fn unscale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * 96 + (dpi.max(1) as i64 / 2)) / dpi.max(1) as i64) as i32
}

fn report_line_count(report: &str) -> usize {
    if report.trim().is_empty() {
        0
    } else {
        report.lines().count()
    }
}

/// Dedicated row commands must use the same owner-drawn Inno button path as dialog commands.
/// Their command routing remains content-owned so activating Browse never hides the dialog.
fn content_command_button_style() -> i32 {
    BS_OWNERDRAW | WS_TABSTOP.0 as i32
}

fn software_export_button_geometry(
    client_width: i32,
    dpi: u32,
    current_width: i32,
    cancel_left: Option<i32>,
) -> (i32, i32) {
    let minimum_width = scale(132, dpi);
    let width = current_width.max(minimum_width).min(client_width.max(0));
    let right = cancel_left
        .map(|left| left - scale(10, dpi))
        .unwrap_or_else(|| client_width - scale(12, dpi));
    ((right - width).max(0), width)
}

unsafe fn child_rect_in_parent(control: HWND, parent: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    if GetWindowRect(control, &mut rect).is_err() {
        return None;
    }
    let mut top_left = POINT {
        x: rect.left,
        y: rect.top,
    };
    let mut bottom_right = POINT {
        x: rect.right,
        y: rect.bottom,
    };
    if !ScreenToClient(parent, &mut top_left).as_bool()
        || !ScreenToClient(parent, &mut bottom_right).as_bool()
    {
        return None;
    }
    Some(RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    if !control.is_invalid() {
        let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
    }
}

unsafe fn set_text(control: HWND, value: &str) {
    if !control.is_invalid() {
        let value = wide(value);
        let _ = SetWindowTextW(control, PCWSTR(value.as_ptr()));
    }
}

unsafe fn initialize_software_columns(list: HWND) {
    for (index, (title, width)) in [
        (crate::tr!("软件名称"), 360),
        (crate::tr!("版本"), 130),
        (crate::tr!("发布者"), 220),
    ]
    .into_iter()
    .enumerate()
    {
        let mut title = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: width,
            pszText: PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn set_software_column_widths(list: HWND, width: i32) {
    if list.is_invalid() || width <= 0 {
        return;
    }
    let widths = [width * 52 / 100, width * 18 / 100, width * 30 / 100];
    for (column, width) in widths.into_iter().enumerate() {
        let _ = SendMessageW(
            list,
            LVM_SETCOLUMNWIDTH,
            WPARAM(column),
            LPARAM(width.max(60) as isize),
        );
    }
}

unsafe fn refill_software_list(list: HWND, state: &SoftwareListState) {
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    if state.loading {
        insert_software_cell(list, 0, 0, &crate::tr!("正在读取已安装软件…"));
        return;
    }
    if !state.records.is_empty() {
        for (row, record) in state.records.iter().enumerate() {
            insert_software_cell(list, row, 0, &record.name);
            insert_software_cell(list, row, 1, &record.version);
            insert_software_cell(list, row, 2, &record.publisher);
        }
    } else {
        for (row, value) in state.rows.iter().enumerate() {
            insert_software_cell(list, row, 0, value);
        }
    }
}

unsafe fn insert_software_cell(list: HWND, row: usize, column: usize, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row as i32,
        iSubItem: column as i32,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let message = if column == 0 {
        LVM_INSERTITEMW
    } else {
        LVM_SETITEMTEXTW
    };
    let _ = SendMessageW(
        list,
        message,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn get_text(control: HWND) -> String {
    if control.is_invalid() {
        return String::new();
    }
    let length = GetWindowTextLengthW(control);
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = GetWindowTextW(control, &mut buffer);
    String::from_utf16_lossy(&buffer[..copied as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_tool_reports_keep_scrollable_dark_theme_role() {
        for kind in [
            ToolDialogKind::NetworkInformation,
            ToolDialogKind::ReadGhoPassword,
            ToolDialogKind::VerifyImage,
            ToolDialogKind::VerifyFileHash,
        ] {
            assert_eq!(
                report_control_kind(kind),
                Some(NativeControlKind::ScrollableField)
            );
        }
        assert_eq!(report_control_kind(ToolDialogKind::SoftwareList), None);
    }

    #[test]
    fn every_dialog_has_compact_inno_commands() {
        for kind in [
            ToolDialogKind::NetworkInformation,
            ToolDialogKind::SoftwareList,
            ToolDialogKind::ReadGhoPassword,
            ToolDialogKind::VerifyImage,
            ToolDialogKind::VerifyFileHash,
        ] {
            let spec = dialog_spec(kind);
            assert!(spec.width <= 760);
            assert!(spec.height <= 540);
            assert_eq!(spec.buttons.cancel, Some(crate::tr!("关闭")));
        }
        assert_eq!(dialog_spec(ToolDialogKind::ReadGhoPassword).height, 350);
        assert_eq!(dialog_spec(ToolDialogKind::VerifyImage).height, 400);
        assert_eq!(dialog_spec(ToolDialogKind::VerifyFileHash).height, 420);
        assert!(dialog_spec(ToolDialogKind::NetworkInformation)
            .buttons
            .secondary
            .is_none());
        assert!(dialog_spec(ToolDialogKind::SoftwareList)
            .buttons
            .secondary
            .is_none());
        for kind in [
            ToolDialogKind::ReadGhoPassword,
            ToolDialogKind::VerifyImage,
            ToolDialogKind::VerifyFileHash,
        ] {
            assert!(dialog_spec(kind).buttons.secondary.is_none());
        }
    }

    #[test]
    fn file_tools_use_a_non_closing_content_browse_command() {
        for kind in [
            ToolDialogKind::ReadGhoPassword,
            ToolDialogKind::VerifyImage,
            ToolDialogKind::VerifyFileHash,
        ] {
            assert!(dialog_spec(kind).buttons.secondary.is_none());
        }
        assert_eq!(
            browse_intent(ToolDialogKind::ReadGhoPassword),
            Some(ToolDialogIntent::BrowseGhoImage)
        );
        assert_eq!(
            browse_intent(ToolDialogKind::VerifyImage),
            Some(ToolDialogIntent::BrowseImageForVerification)
        );
        assert_eq!(
            browse_intent(ToolDialogKind::VerifyFileHash),
            Some(ToolDialogIntent::BrowseFileForHash)
        );
        assert_eq!(browse_intent(ToolDialogKind::NetworkInformation), None);
        assert_eq!(content_command_button_style() & 0x0f, BS_OWNERDRAW);
        assert_ne!(content_command_button_style() & WS_TABSTOP.0 as i32, 0);
    }

    #[test]
    fn software_export_command_keeps_caption_width_and_standard_gaps_at_high_dpi() {
        for dpi in [96, 144, 192] {
            let client = scale(760, dpi);
            let minimum = scale(132, dpi);
            let (x, width) = software_export_button_geometry(client, dpi, scale(75, dpi), None);
            assert_eq!(width, minimum);
            assert_eq!(x + width, client - scale(12, dpi));

            let cancel_left = client - scale(12 + 75, dpi);
            let (x, width) =
                software_export_button_geometry(client, dpi, scale(75, dpi), Some(cancel_left));
            assert_eq!(width, minimum);
            assert_eq!(x + width + scale(10, dpi), cancel_left);
        }
    }

    #[test]
    fn progress_is_bounded_and_idle_result_is_unchanged() {
        assert_eq!(progress_report(true, 255, "ignored"), "正在校验… 100%");
        assert_eq!(progress_report(false, 0, "SHA-256: abc"), "SHA-256: abc");
    }

    #[test]
    fn control_ids_stay_in_the_reserved_dialog_range() {
        assert_eq!(control_id(0), 63_100);
        assert_eq!(control_id(4), 63_104);
    }

    #[test]
    fn report_density_counts_visible_lines_without_allocating_empty_rows() {
        assert_eq!(report_line_count(""), 0);
        assert_eq!(report_line_count("  \r\n\t"), 0);
        assert_eq!(report_line_count("one"), 1);
        assert_eq!(report_line_count("one\r\ntwo\r\nthree"), 3);
    }

    #[test]
    fn physical_content_heights_round_trip_through_dialog_logical_units() {
        for dpi in [96, 120, 144, 192] {
            for logical in [90, 134, 200, 420] {
                let physical = scale(logical, dpi);
                assert!((unscale(physical, dpi) - logical).abs() <= 1);
            }
        }
    }

    #[test]
    fn structured_software_export_preserves_legacy_header_and_count() {
        let text = software_export_text(&SoftwareListState {
            records: vec![
                crate::core::native_tool_executor::InstalledSoftwareRecord {
                    name: "Tool A".to_owned(),
                    version: "1.0".to_owned(),
                    publisher: "Vendor".to_owned(),
                    install_location: "C:\\ToolA".to_owned(),
                },
                crate::core::native_tool_executor::InstalledSoftwareRecord {
                    name: "Tool B".to_owned(),
                    version: String::new(),
                    publisher: String::new(),
                    install_location: String::new(),
                },
            ],
            ..Default::default()
        });
        assert!(text.contains("已安装软件列表 - 导出时间:"));
        assert!(text.contains("软件名称"));
        assert!(text.contains("Tool A"));
        assert!(text.contains("共 2 个软件"));
    }

    #[test]
    fn only_valid_decoded_gho_password_is_copyable() {
        let mut result = crate::core::native_tool_executor::GhoPasswordResult {
            path: "image.gho".to_owned(),
            valid: true,
            has_password: true,
            password: Some("secret".to_owned()),
            password_length: 6,
            error: None,
        };
        assert_eq!(copyable_gho_password(&result).as_deref(), Some("secret"));
        result.valid = false;
        assert_eq!(copyable_gho_password(&result), None);
    }

    #[test]
    fn structured_image_and_hash_results_keep_sizes_and_counts() {
        let image = format_image_result(
            &crate::core::native_tool_executor::ImageVerificationResult {
                path: "install.swm".to_owned(),
                image_type: "SWM".to_owned(),
                status: "Valid".to_owned(),
                valid: true,
                file_size: 2 * 1024 * 1024,
                image_count: 3,
                part_count: 2,
                message: "OK".to_owned(),
                details: vec!["detail".to_owned()],
            },
        );
        assert!(image.contains("2.00 MB"));
        assert!(image.contains("镜像数量：3"));
        assert!(image.contains("分卷数量：2"));

        let hash = format_hash_result(&crate::core::native_tool_executor::Sha256Result {
            path: "file.bin".to_owned(),
            file_size: 1024,
            sha256: "abc".to_owned(),
            expected: "ABC".to_owned(),
            matched: Some(true),
        });
        assert!(hash.contains("1.00 KB"));
        assert!(hash.contains("SHA-256: abc"));
    }
}
