//! Native online-resource page controls.
//!
//! This module only turns Win32 notifications into user intents.  Fetching the
//! remote catalogue and starting a download remain controller responsibilities.

use std::cell::Cell;

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_INSERTCOLUMNW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_REPORT,
    LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, BS_OWNERDRAW, ES_AUTOHSCROLL, SW_HIDE,
    SW_SHOW, WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use crate::core::native_download_controller::ResourceRow;
use crate::native_ui::controls::{child, wide};
use crate::native_ui::theme::{
    apply_control_theme, apply_list_view_theme, NativeControlKind, Palette,
};

pub const ID_TAB_SYSTEM: u16 = 5_000;
pub const ID_TAB_SOFTWARE: u16 = 5_001;
pub const ID_TAB_GPU_DRIVER: u16 = 5_002;
pub const ID_RESOURCE_LIST: u16 = 5_003;
pub const ID_SAVE_PATH: u16 = 5_004;
pub const ID_BROWSE: u16 = 5_005;
pub const ID_REFRESH: u16 = 5_006;
pub const ID_DOWNLOAD: u16 = 5_007;
pub const ID_INSTALL: u16 = 5_008;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadTab {
    #[default]
    SystemImage,
    Software,
    GpuDriver,
}

unsafe fn insert_item(list: HWND, index: i32, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: index,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        0x104D,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn set_subitem(list: HWND, row: i32, column: i32, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        iSubItem: column,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        0x104C,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

/// A side-effect-free request emitted by the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadIntent {
    SelectTab(DownloadTab),
    BrowseSaveFolder,
    RefreshCatalogue,
    DownloadSelected,
    InstallSelected,
}

pub struct DownloadLabels<'a> {
    pub system_tab: &'a str,
    pub software_tab: &'a str,
    pub gpu_driver_tab: &'a str,
    pub status_ready: &'a str,
    pub name_column: &'a str,
    pub type_column: &'a str,
    pub size_column: &'a str,
    pub save_path: &'a str,
    pub browse: &'a str,
    pub refresh: &'a str,
    pub download: &'a str,
    pub install: &'a str,
}

/// Coordinates are logical pixels and are scaled by the caller-provided DPI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DownloadVerticalLayout {
    list_y: i32,
    list_height: i32,
    field_y: i32,
    actions_y: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DownloadColumnWidths {
    name: i32,
    resource_type: i32,
    size: i32,
}

fn download_column_widths(tab: DownloadTab, list_width: i32, dpi: u32) -> DownloadColumnWidths {
    let s = |value: i32| value * dpi.max(1) as i32 / 96;
    // Leave room for the vertical scrollbar and the list border.  The trailing
    // column is deliberately compact: catalogue names benefit most from the
    // available width, especially after translation or at low resolutions.
    let usable_width = (list_width - s(20)).max(0);
    let desired_trailing_width = match tab {
        DownloadTab::SystemImage => s(112),
        DownloadTab::Software | DownloadTab::GpuDriver => s(132),
    };
    let trailing_width = desired_trailing_width.min(usable_width * 2 / 5);
    let name_width = (usable_width - trailing_width).max(0);

    match tab {
        DownloadTab::SystemImage => DownloadColumnWidths {
            name: name_width,
            resource_type: trailing_width,
            size: 0,
        },
        DownloadTab::Software | DownloadTab::GpuDriver => DownloadColumnWidths {
            name: name_width,
            resource_type: 0,
            size: trailing_width,
        },
    }
}

fn download_vertical_layout(rect: PageRect, dpi: u32) -> DownloadVerticalLayout {
    let s = |value: i32| value * dpi.max(1) as i32 / 96;
    let height = rect.height.max(0);
    let button_height = s(30).min(height);
    let gap = s(8);
    let page_bottom = rect.y + height;
    let list_y = (rect.y + button_height + s(10) + s(24)).min(page_bottom);
    let actions_y = (page_bottom - button_height).max(rect.y);
    let field_y = (actions_y - gap - button_height).max(rect.y);
    DownloadVerticalLayout {
        list_y,
        list_height: (field_y - gap - list_y).max(0),
        field_y,
        actions_y,
    }
}

pub struct DownloadPage {
    pub tabs: [HWND; 3],
    pub status: HWND,
    pub resources: HWND,
    pub save_path_label: HWND,
    pub save_path: HWND,
    pub browse: HWND,
    pub refresh: HWND,
    pub download: HWND,
    pub install: HWND,
    selected_tab: DownloadTab,
    list_width: Cell<i32>,
    dpi: Cell<u32>,
}

impl DownloadPage {
    /// Creates hidden children. `show(true)` is required when the route becomes active.
    pub unsafe fn create(
        parent: HWND,
        font: HFONT,
        labels: &DownloadLabels<'_>,
    ) -> windows::core::Result<Self> {
        let tabs = [
            child(
                parent,
                w!("BUTTON"),
                labels.system_tab,
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                ID_TAB_SYSTEM,
            )?,
            child(
                parent,
                w!("BUTTON"),
                labels.software_tab,
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                ID_TAB_SOFTWARE,
            )?,
            child(
                parent,
                w!("BUTTON"),
                labels.gpu_driver_tab,
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                ID_TAB_GPU_DRIVER,
            )?,
        ];
        let status = child(parent, w!("STATIC"), labels.status_ready, 0, 5_020)?;
        let resources = child(
            parent,
            w!("SysListView32"),
            "",
            (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
            ID_RESOURCE_LIST,
        )?;
        let _ = SendMessageW(
            resources,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT) as isize),
        );
        insert_columns(resources, labels);

        let save_path_label = child(parent, w!("STATIC"), labels.save_path, 0, 5_021)?;
        let save_path = child(
            parent,
            w!("EDIT"),
            "",
            WS_BORDER.0 as i32 | WS_TABSTOP.0 as i32 | ES_AUTOHSCROLL,
            ID_SAVE_PATH,
        )?;
        let browse = child(
            parent,
            w!("BUTTON"),
            labels.browse,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_BROWSE,
        )?;
        let refresh = child(
            parent,
            w!("BUTTON"),
            labels.refresh,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_REFRESH,
        )?;
        let download = child(
            parent,
            w!("BUTTON"),
            labels.download,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_DOWNLOAD,
        )?;
        let install = child(
            parent,
            w!("BUTTON"),
            labels.install,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_INSTALL,
        )?;

        let page = Self {
            tabs,
            status,
            resources,
            save_path_label,
            save_path,
            browse,
            refresh,
            download,
            install,
            selected_tab: DownloadTab::SystemImage,
            list_width: Cell::new(0),
            dpi: Cell::new(96),
        };
        page.apply_font(font);
        page.show(false);
        Ok(page)
    }

    pub fn selected_tab(&self) -> DownloadTab {
        self.selected_tab
    }

    pub fn select_tab(&mut self, tab: DownloadTab) {
        self.selected_tab = tab;
    }

    pub unsafe fn relocalize(&self, labels: &DownloadLabels<'_>) {
        for (control, label) in self.tabs.into_iter().zip([
            labels.system_tab,
            labels.software_tab,
            labels.gpu_driver_tab,
        ]) {
            set_text(control, label);
        }
        set_text(self.status, labels.status_ready);
        set_text(self.save_path_label, labels.save_path);
        set_text(self.browse, labels.browse);
        set_text(self.refresh, labels.refresh);
        set_text(self.download, labels.download);
        set_text(self.install, labels.install);
        update_columns(self.resources, labels);
    }

    pub unsafe fn replace_rows(&self, rows: &[ResourceRow]) {
        let _ = SendMessageW(self.resources, 0x1009, WPARAM(0), LPARAM(0));
        for (row_index, row) in rows.iter().enumerate() {
            insert_item(self.resources, row_index as i32, &row.name);
            set_subitem(self.resources, row_index as i32, 1, &row.resource_type);
            set_subitem(self.resources, row_index as i32, 2, &row.size);
        }
        self.update_column_widths(self.list_width.get(), self.dpi.get());
    }

    pub unsafe fn selected_resource(&self) -> Option<usize> {
        usize::try_from(SendMessageW(self.resources, 0x100C, WPARAM(usize::MAX), LPARAM(2)).0).ok()
    }

    /// Returns an intent only; the caller owns all network and task state changes.
    pub fn command_intent(command_id: u16) -> Option<DownloadIntent> {
        match command_id {
            ID_TAB_SYSTEM => Some(DownloadIntent::SelectTab(DownloadTab::SystemImage)),
            ID_TAB_SOFTWARE => Some(DownloadIntent::SelectTab(DownloadTab::Software)),
            ID_TAB_GPU_DRIVER => Some(DownloadIntent::SelectTab(DownloadTab::GpuDriver)),
            ID_BROWSE => Some(DownloadIntent::BrowseSaveFolder),
            ID_REFRESH => Some(DownloadIntent::RefreshCatalogue),
            ID_DOWNLOAD => Some(DownloadIntent::DownloadSelected),
            ID_INSTALL => Some(DownloadIntent::InstallSelected),
            _ => None,
        }
    }

    pub unsafe fn layout(&self, rect: PageRect, dpi: u32) {
        let s = |value: i32| value * dpi as i32 / 96;
        let width = rect.width.max(0);
        let height = rect.height.max(0);
        let gap = s(8);
        let tab_width = ((width - gap * 2) / 3).max(0);
        let button_height = s(30).min(height);
        for (index, tab) in self.tabs.iter().copied().enumerate() {
            let _ = MoveWindow(
                tab,
                rect.x + index as i32 * (tab_width + gap),
                rect.y,
                tab_width,
                button_height,
                true,
            );
        }
        let page_bottom = rect.y + height;
        let status_y = (rect.y + button_height + s(10)).min(page_bottom);
        let _ = MoveWindow(
            self.status,
            rect.x,
            status_y,
            width,
            s(22).min((page_bottom - status_y).max(0)),
            true,
        );

        let vertical = download_vertical_layout(rect, dpi);
        self.list_width.set(width);
        self.dpi.set(dpi);
        let _ = MoveWindow(
            self.resources,
            rect.x,
            vertical.list_y,
            width,
            vertical.list_height,
            true,
        );
        self.update_column_widths(width, dpi);

        let label_width = s(72).min(width / 3);
        let browse_width = s(82).min(width / 4);
        let _ = MoveWindow(
            self.save_path_label,
            rect.x,
            vertical.field_y + s(5),
            label_width,
            s(22),
            true,
        );
        let edit_x = rect.x + label_width;
        let edit_width = (width - label_width - browse_width - gap).max(0);
        let _ = MoveWindow(
            self.save_path,
            edit_x,
            vertical.field_y,
            edit_width,
            button_height,
            true,
        );
        let _ = MoveWindow(
            self.browse,
            edit_x + edit_width + gap,
            vertical.field_y,
            browse_width,
            button_height,
            true,
        );

        let action_width = s(112).min((width - gap * 2) / 3).max(0);
        let _ = MoveWindow(
            self.refresh,
            rect.x,
            vertical.actions_y,
            action_width,
            button_height,
            true,
        );
        let _ = MoveWindow(
            self.install,
            rect.x + width - action_width,
            vertical.actions_y,
            action_width,
            button_height,
            true,
        );
        let _ = MoveWindow(
            self.download,
            rect.x + width - action_width * 2 - gap,
            vertical.actions_y,
            action_width,
            button_height,
            true,
        );
    }

    pub unsafe fn show(&self, visible: bool) {
        for hwnd in self.controls() {
            let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    pub unsafe fn apply_font(&self, font: HFONT) {
        for hwnd in self.controls() {
            let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        let _ = apply_list_view_theme(self.resources, palette);
        for control in self.tabs.iter().copied().chain([
            self.browse,
            self.refresh,
            self.download,
            self.install,
        ]) {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        apply_control_theme(self.save_path, palette, NativeControlKind::Field);
        let _ = SendMessageW(
            self.resources,
            0x1001,
            WPARAM(0),
            LPARAM(palette.edit.0 as isize),
        );
        let _ = SendMessageW(
            self.resources,
            0x1026,
            WPARAM(0),
            LPARAM(palette.edit.0 as isize),
        );
        let _ = SendMessageW(
            self.resources,
            0x1024,
            WPARAM(0),
            LPARAM(palette.text.0 as isize),
        );
    }

    fn controls(&self) -> impl Iterator<Item = HWND> + '_ {
        self.tabs.iter().copied().chain([
            self.status,
            self.resources,
            self.save_path_label,
            self.save_path,
            self.browse,
            self.refresh,
            self.download,
            self.install,
        ])
    }

    unsafe fn update_column_widths(&self, list_width: i32, dpi: u32) {
        let widths = download_column_widths(self.selected_tab, list_width, dpi);
        for (index, width) in [widths.name, widths.resource_type, widths.size]
            .into_iter()
            .enumerate()
        {
            let _ = SendMessageW(
                self.resources,
                0x101E, // LVM_SETCOLUMNWIDTH
                WPARAM(index),
                LPARAM(width as isize),
            );
        }
    }
}

unsafe fn insert_columns(list: HWND, labels: &DownloadLabels<'_>) {
    for (index, (title, width)) in [
        (labels.name_column, 330),
        (labels.type_column, 130),
        (labels.size_column, 110),
    ]
    .into_iter()
    .enumerate()
    {
        let mut text = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: width,
            pszText: PWSTR(text.as_mut_ptr()),
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

unsafe fn update_columns(list: HWND, labels: &DownloadLabels<'_>) {
    for (index, title) in [labels.name_column, labels.type_column, labels.size_column]
        .into_iter()
        .enumerate()
    {
        let mut text = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            0x1060,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn set_text(control: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(control, windows::core::PCWSTR(value.as_ptr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_mapped_without_starting_work() {
        assert_eq!(
            DownloadPage::command_intent(ID_TAB_SOFTWARE),
            Some(DownloadIntent::SelectTab(DownloadTab::Software))
        );
        assert_eq!(
            DownloadPage::command_intent(ID_REFRESH),
            Some(DownloadIntent::RefreshCatalogue)
        );
        assert_eq!(
            DownloadPage::command_intent(ID_INSTALL),
            Some(DownloadIntent::InstallSelected)
        );
        assert_eq!(DownloadPage::command_intent(4_999), None);
    }

    #[test]
    fn footer_is_anchored_inside_low_height_pages_at_100_and_200_percent() {
        for (rect, dpi) in [
            (
                PageRect {
                    x: 0,
                    y: 0,
                    width: 520,
                    height: 220,
                },
                96,
            ),
            (
                PageRect {
                    x: 20,
                    y: 30,
                    width: 1_040,
                    height: 440,
                },
                192,
            ),
        ] {
            let layout = download_vertical_layout(rect, dpi);
            let button_height = 30 * dpi as i32 / 96;
            assert!(layout.actions_y + button_height <= rect.y + rect.height);
            assert!(layout.field_y <= layout.actions_y);
            assert!(layout.list_height >= 0);
        }
    }

    #[test]
    fn catalogue_columns_hide_fields_that_have_no_meaning_for_the_active_tab() {
        let system = download_column_widths(DownloadTab::SystemImage, 960, 96);
        assert!(system.name > system.resource_type);
        assert!(system.resource_type > 0);
        assert_eq!(system.size, 0);

        for tab in [DownloadTab::Software, DownloadTab::GpuDriver] {
            let widths = download_column_widths(tab, 960, 96);
            assert!(widths.name > widths.size);
            assert_eq!(widths.resource_type, 0);
            assert!(widths.size > 0);
        }
    }

    #[test]
    fn catalogue_columns_stay_non_negative_at_narrow_high_dpi_sizes() {
        for tab in [
            DownloadTab::SystemImage,
            DownloadTab::Software,
            DownloadTab::GpuDriver,
        ] {
            let widths = download_column_widths(tab, 48, 192);
            assert!(widths.name >= 0);
            assert!(widths.resource_type >= 0);
            assert!(widths.size >= 0);
        }
    }
}
