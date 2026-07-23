//! Dedicated native driver export/import dialog.
//!
//! The dialog only edits state and emits typed intents. Directory browsing and driver operations
//! remain owned by the main window and the existing confirmed tool execution boundary.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW, ShowWindow,
    BM_GETCHECK, BM_SETCHECK, BS_AUTORADIOBUTTON, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOHSCROLL, SW_HIDE, SW_SHOW, WM_SETFONT,
    WS_GROUP, WS_TABSTOP,
};

use super::controls::{child, wide};
use super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::layout::{measure_text, measured_button_width, LayoutMetrics};
use super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::native_driver_transfer::{
    build_execute_intent, DriverTransferIntent, DriverTransferMode, DriverTransferState,
};

const ID_MODE_EXPORT: u16 = 62_700;
const ID_MODE_IMPORT: u16 = 62_701;
const ID_WINDOWS_TARGET: u16 = 62_702;
const ID_DIRECTORY: u16 = 62_703;
pub const ID_BROWSE_DIRECTORY: u16 = 62_704;
const RADIO_CONTROL_KIND: NativeControlKind = NativeControlKind::General;

struct DriverTransferControls {
    export_mode: HWND,
    import_mode: HWND,
    windows_label: HWND,
    windows_target: HWND,
    directory_label: HWND,
    directory: HWND,
    browse: HWND,
    status: HWND,
}

pub struct NativeDriverTransferDialog {
    shell: DialogShell,
    controls: DriverTransferControls,
    state: DriverTransferState,
    font: HFONT,
}

impl NativeDriverTransferDialog {
    pub unsafe fn create(owner: HWND, state: DriverTransferState) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("驱动备份还原"),
                title: crate::tr!("驱动备份还原"),
                description: crate::tr!("导出或导入系统驱动"),
                width: 610,
                height: 320,
                buttons: DialogButtons {
                    primary: crate::tr!("执行"),
                    secondary: None,
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
        let parent = shell.content();
        let export_mode = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("导出驱动"),
            BS_AUTORADIOBUTTON | WS_GROUP.0 as i32 | WS_TABSTOP.0 as i32,
            ID_MODE_EXPORT,
        )?;
        let import_mode = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("导入驱动"),
            BS_AUTORADIOBUTTON | WS_TABSTOP.0 as i32,
            ID_MODE_IMPORT,
        )?;
        let windows_label = child(parent, w!("STATIC"), "", 0, 62_705)?;
        let windows_target = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_WINDOWS_TARGET,
        )?;
        let directory_label = child(parent, w!("STATIC"), "", 0, 62_706)?;
        let directory = child(
            parent,
            w!("EDIT"),
            "",
            ES_AUTOHSCROLL | WS_TABSTOP.0 as i32,
            ID_DIRECTORY,
        )?;
        let browse = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("浏览..."),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_BROWSE_DIRECTORY,
        )?;
        let status = child(parent, w!("STATIC"), "", 0, 62_707)?;
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
            controls: DriverTransferControls {
                export_mode,
                import_mode,
                windows_label,
                windows_target,
                directory_label,
                directory,
                browse,
                status,
            },
            state,
            font,
        };
        dialog.apply_font();
        dialog.apply_theme(dialog.shell.palette());
        dialog.refresh_controls();
        dialog.layout();
        Ok(dialog)
    }

    pub fn hwnd(&self) -> HWND {
        self.shell.hwnd()
    }

    pub unsafe fn activate_if_visible(&self) -> bool {
        self.shell.activate_if_visible()
    }

    pub fn state(&self) -> &DriverTransferState {
        &self.state
    }

    pub unsafe fn set_state(&mut self, state: DriverTransferState) {
        self.state = state;
        self.refresh_controls();
    }

    pub unsafe fn set_directory(&mut self, directory: &str) {
        self.state.directory = directory.trim().to_owned();
        set_text(self.controls.directory, &self.state.directory);
        self.update_primary_enabled();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.refresh_controls();
        self.layout();
        self.shell.show_modeless();
    }

    /// Synchronizes radio, target and directory controls. Browse remains a returned intent.
    pub unsafe fn handle_command(&mut self, control_id: u16) -> bool {
        match control_id {
            ID_MODE_EXPORT => {
                self.state.mode = DriverTransferMode::Export;
                self.refresh_mode_controls();
            }
            ID_MODE_IMPORT => {
                self.state.mode = DriverTransferMode::Import;
                self.refresh_mode_controls();
            }
            ID_WINDOWS_TARGET | ID_DIRECTORY => self.sync_state_from_controls(),
            _ => return false,
        }
        self.state.status.clear();
        set_text(self.controls.status, "");
        self.layout();
        self.update_primary_enabled();
        true
    }

    pub fn intent_for_command(&self, control_id: u16) -> Option<DriverTransferIntent> {
        (control_id == ID_BROWSE_DIRECTORY)
            .then(|| DriverTransferIntent::BrowseDirectory(self.state.directory_role()))
    }

    pub unsafe fn take_intent(&mut self) -> Option<DriverTransferIntent> {
        match self.shell.take_result()? {
            DialogResult::Primary => {
                self.sync_state_from_controls();
                match build_execute_intent(&self.state) {
                    Ok(intent) => Some(intent),
                    Err(error) => {
                        self.state.status = error.to_string();
                        set_text(self.controls.status, &self.state.status);
                        self.layout();
                        self.shell.show_modeless();
                        None
                    }
                }
            }
            DialogResult::Secondary | DialogResult::Cancel => Some(DriverTransferIntent::Close),
        }
    }

    unsafe fn sync_state_from_controls(&mut self) {
        self.state.mode = if is_checked(self.controls.import_mode) {
            DriverTransferMode::Import
        } else {
            DriverTransferMode::Export
        };
        let selected = SendMessageW(
            self.controls.windows_target,
            CB_GETCURSEL,
            WPARAM(0),
            LPARAM(0),
        )
        .0;
        self.state.selected_windows = usize::try_from(selected)
            .ok()
            .and_then(|index| self.state.windows_targets.get(index))
            .map(|entry| entry.value.clone());
        self.state.directory = get_text(self.controls.directory).trim().to_owned();
    }

    unsafe fn refresh_controls(&mut self) {
        self.refresh_mode_controls();
        let _ = SendMessageW(
            self.controls.windows_target,
            CB_RESETCONTENT,
            WPARAM(0),
            LPARAM(0),
        );
        let mut selected = None;
        for (index, entry) in self.state.windows_targets.iter().enumerate() {
            let label = wide(&entry.label);
            let _ = SendMessageW(
                self.controls.windows_target,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(label.as_ptr() as isize),
            );
            if self
                .state
                .selected_windows
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&entry.value))
            {
                selected = Some(index);
            }
        }
        if let Some(index) = selected {
            let _ = SendMessageW(
                self.controls.windows_target,
                CB_SETCURSEL,
                WPARAM(index),
                LPARAM(0),
            );
        }
        set_text(self.controls.directory, &self.state.directory);
        let status = if self.state.inventory_loading {
            crate::tr!("正在检测Windows分区...")
        } else {
            self.state.status.clone()
        };
        set_text(self.controls.status, &status);
        self.layout();
        self.update_primary_enabled();
    }

    unsafe fn refresh_mode_controls(&self) {
        set_checked(
            self.controls.export_mode,
            self.state.mode == DriverTransferMode::Export,
        );
        set_checked(
            self.controls.import_mode,
            self.state.mode == DriverTransferMode::Import,
        );
        let (windows_label, directory_label) = match self.state.mode {
            DriverTransferMode::Export => (crate::tr!("源系统分区:"), crate::tr!("保存目录:")),
            DriverTransferMode::Import => (crate::tr!("目标系统分区:"), crate::tr!("驱动目录:")),
        };
        set_text(self.controls.windows_label, &windows_label);
        set_text(self.controls.directory_label, &directory_label);
    }

    unsafe fn update_primary_enabled(&self) {
        self.shell
            .set_primary_enabled(build_execute_intent(&self.state).is_ok());
    }

    unsafe fn apply_theme(&self, palette: Palette) {
        for radio in [self.controls.export_mode, self.controls.import_mode] {
            apply_control_theme(radio, palette, RADIO_CONTROL_KIND);
        }
        for control in [
            self.controls.browse,
            self.controls.windows_label,
            self.controls.directory_label,
            self.controls.status,
        ] {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        apply_control_theme(
            self.controls.windows_target,
            palette,
            NativeControlKind::Field,
        );
        apply_control_theme(self.controls.directory, palette, NativeControlKind::Field);
    }

    unsafe fn apply_font(&self) {
        for control in [
            self.controls.export_mode,
            self.controls.import_mode,
            self.controls.windows_label,
            self.controls.windows_target,
            self.controls.directory_label,
            self.controls.directory,
            self.controls.browse,
            self.controls.status,
        ] {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
    }

    unsafe fn layout(&mut self) {
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let width = rect.right.max(0);
        let status_visible = GetWindowTextLengthW(self.controls.status) > 0;
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_width = [
            get_text(self.controls.windows_label),
            get_text(self.controls.directory_label),
        ]
        .iter()
        .map(|label| measure_text(self.shell.hwnd(), self.font, label, None).width)
        .max()
        .unwrap_or(0)
            + metrics.control_gap;
        let browse_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("浏览..."),
            dpi,
            scale(75, dpi),
        );
        let status_height = if status_visible {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &get_text(self.controls.status),
                Some(width),
            )
            .height
            .max(metrics.label_height)
        } else {
            0
        };
        let desired_height = metrics.label_height
            + metrics.section_gap
            + metrics.field_height
            + metrics.control_gap
            + metrics.field_height
            + if status_visible {
                metrics.section_gap + status_height
            } else {
                0
            };
        self.shell.fit_content_height(unscale(desired_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let height = rect.bottom.max(0);
        let layout = DriverTransferContentLayout::calculate_with_widths(
            width,
            height,
            dpi,
            status_visible,
            label_width,
            browse_width,
            status_height,
        );
        let _ = MoveWindow(
            self.controls.export_mode,
            0,
            0,
            layout.mode_width,
            layout.row_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.import_mode,
            layout.mode_width + layout.gap,
            0,
            layout.mode_width,
            layout.row_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.windows_label,
            0,
            layout.target_y + layout.label_offset,
            layout.label_width,
            layout.row_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.windows_target,
            layout.label_width,
            layout.target_y,
            (width - layout.label_width).max(0),
            scale(220, dpi),
            true,
        );
        let _ = MoveWindow(
            self.controls.directory_label,
            0,
            layout.directory_y + layout.label_offset,
            layout.label_width,
            layout.row_height,
            true,
        );
        let edit_width = (width - layout.label_width - layout.gap - layout.browse_width).max(0);
        let _ = MoveWindow(
            self.controls.directory,
            layout.label_width,
            layout.directory_y,
            edit_width,
            layout.field_height,
            true,
        );
        let _ = MoveWindow(
            self.controls.browse,
            layout.label_width + edit_width + layout.gap,
            layout.directory_y,
            layout.browse_width,
            layout.field_height,
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
        let _ = ShowWindow(
            self.controls.status,
            if status_visible { SW_SHOW } else { SW_HIDE },
        );
        let _ = ShowWindow(self.controls.windows_target, SW_SHOW);
        let _ = ShowWindow(self.controls.directory, SW_SHOW);
        let _ = ShowWindow(self.controls.browse, SW_SHOW);
        let _ = ShowWindow(self.controls.export_mode, SW_SHOW);
        let _ = ShowWindow(self.controls.import_mode, SW_SHOW);
        let _ = ShowWindow(self.controls.windows_label, SW_SHOW);
        let _ = ShowWindow(self.controls.directory_label, SW_SHOW);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DriverTransferContentLayout {
    mode_width: i32,
    row_height: i32,
    field_height: i32,
    label_width: i32,
    browse_width: i32,
    gap: i32,
    label_offset: i32,
    target_y: i32,
    directory_y: i32,
    status_y: i32,
    status_height: i32,
}

impl DriverTransferContentLayout {
    fn calculate(width: i32, height: i32, dpi: u32, status_visible: bool) -> Self {
        Self::calculate_with_widths(
            width,
            height,
            dpi,
            status_visible,
            scale(104, dpi).min(width.max(0) / 3),
            scale(88, dpi).min(width.max(0) / 3),
            scale(40, dpi),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn calculate_with_widths(
        width: i32,
        height: i32,
        dpi: u32,
        status_visible: bool,
        measured_label_width: i32,
        measured_browse_width: i32,
        measured_status_height: i32,
    ) -> Self {
        let width = width.max(0);
        let height = height.max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let row_height = metrics.label_height;
        let field_height = metrics.field_height;
        let label_width = measured_label_width.clamp(0, width / 3);
        let browse_width = measured_browse_width.clamp(0, width / 3);
        let gap = metrics.control_gap;
        let label_offset = ((field_height - row_height) / 2).max(0);
        let target_y = row_height + metrics.section_gap;
        let directory_y = target_y + field_height + gap;
        let status_y = directory_y + field_height + metrics.section_gap;
        let status_height = if status_visible {
            measured_status_height.min((height - status_y).max(0))
        } else {
            0
        };
        Self {
            mode_width: scale(150, dpi).min((width - gap).max(0) / 2),
            row_height,
            field_height,
            label_width,
            browse_width,
            gap,
            label_offset,
            target_y,
            directory_y,
            status_y,
            status_height,
        }
    }
}

impl Drop for NativeDriverTransferDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn set_checked(control: HWND, checked: bool) {
    let _ = SendMessageW(
        control,
        BM_SETCHECK,
        WPARAM(usize::from(checked)),
        LPARAM(0),
    );
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn unscale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

unsafe fn is_checked(control: HWND) -> bool {
    SendMessageW(control, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1
}

unsafe fn set_text(control: HWND, value: &str) {
    let value = wide(value);
    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
        control,
        windows::core::PCWSTR(value.as_ptr()),
    );
}

unsafe fn get_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_mode_radios_keep_native_general_theme_role() {
        assert_eq!(RADIO_CONTROL_KIND, NativeControlKind::General);
    }

    #[test]
    fn browse_control_uses_the_mode_specific_directory_role() {
        let mut state = DriverTransferState::default();
        assert_eq!(
            DriverTransferIntent::BrowseDirectory(state.directory_role()),
            DriverTransferIntent::BrowseDirectory(
                crate::core::native_driver_transfer::DriverDirectoryRole::ExportDestination
            )
        );
        state.mode = DriverTransferMode::Import;
        assert_eq!(
            DriverTransferIntent::BrowseDirectory(state.directory_role()),
            DriverTransferIntent::BrowseDirectory(
                crate::core::native_driver_transfer::DriverDirectoryRole::ImportSource
            )
        );
    }

    #[test]
    fn driver_dialog_control_ids_do_not_overlap_standard_dialog_commands() {
        let ids = [
            ID_MODE_EXPORT,
            ID_MODE_IMPORT,
            ID_WINDOWS_TARGET,
            ID_DIRECTORY,
            ID_BROWSE_DIRECTORY,
        ];
        assert_eq!(
            ids.into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
    }

    #[test]
    fn compact_driver_layout_hides_empty_status_and_stays_within_content() {
        for dpi in [96, 144, 192] {
            let width = scale(554, dpi);
            let height = scale(160, dpi);
            let empty = DriverTransferContentLayout::calculate(width, height, dpi, false);
            assert_eq!(empty.status_height, 0);
            assert!(empty.directory_y + empty.field_height <= height);

            let message = DriverTransferContentLayout::calculate(width, height, dpi, true);
            assert!(message.status_height > 0);
            assert!(message.status_y + message.status_height <= height);
            assert!(message.mode_width * 2 + message.gap <= width);
        }
    }
}
