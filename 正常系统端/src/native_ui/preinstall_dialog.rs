//! Native category/checklist dialog for selecting applications installed after Windows setup.
//!
//! The dialog is presentation-only. It stores selections by the v4 catalogue's stable software
//! id so switching categories never couples a choice to response order. VMware Tools is rejected
//! defensively here because it is exposed as a separate environment-gated advanced option.

use std::cell::Cell;
use std::collections::BTreeSet;

use lr_core::software_install::{validate_selected_packages, SelectedSoftwarePackage};
use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW,
    LVM_DELETEALLITEMS, LVM_GETITEMSTATE, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_CHECKBOXES, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_EX_INFOTIP, LVS_NOCOLUMNHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, WS_BORDER, WS_TABSTOP, WS_VSCROLL,
};

use super::controls::{child, wide};
use super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::layout::LayoutMetrics;
use super::theme::{apply_list_view_theme, Palette};
use super::GetDpiForWindow;
use crate::core::native_download_controller::NativeDownloadController;
use crate::download::config::SoftwareCategory;

pub const ID_PREINSTALL_CATEGORY: u16 = 65_000;
const ID_PREINSTALL_LIST: u16 = 65_001;

const LVM_SETITEMSTATE: u32 = 0x102B;
const LVIS_STATEIMAGEMASK: u32 = 0xF000;
const CHECKED_STATE_IMAGE: u32 = 2 << 12;
const UNCHECKED_STATE_IMAGE: u32 = 1 << 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreinstallDialogIntent {
    Apply(Vec<SelectedSoftwarePackage>),
    Close,
}

pub struct NativePreinstallDialog {
    pub shell: DialogShell,
    categories: Vec<SoftwareCategory>,
    selected_ids: BTreeSet<String>,
    active_category: usize,
    category_list: HWND,
    package_list: HWND,
    selection_write_in_progress: Cell<bool>,
}

impl NativePreinstallDialog {
    pub unsafe fn create(
        owner: HWND,
        categories: Vec<SoftwareCategory>,
        selected: &[SelectedSoftwarePackage],
    ) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("选择预装应用"),
                title: crate::tr!("选择预装应用"),
                description: crate::tr!(
                    "选中的应用将在 Windows 安装完成后按服务器提供的静默参数安装。"
                ),
                width: 780,
                height: 570,
                buttons: DialogButtons {
                    primary: crate::tr!("确定"),
                    secondary: None,
                    cancel: Some(crate::tr!("取消")),
                },
            },
        )?;
        let category_list = child(
            shell.content(),
            w!("SysListView32"),
            "",
            (LVS_REPORT
                | LVS_SHOWSELALWAYS
                | LVS_NOCOLUMNHEADER
                | LVS_SINGLESEL
                | WS_BORDER.0
                | WS_VSCROLL.0
                | WS_TABSTOP.0) as i32,
            ID_PREINSTALL_CATEGORY,
        )?;
        let _ = SendMessageW(
            category_list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT) as isize),
        );
        insert_category_column(category_list);
        let package_list = child(
            shell.content(),
            w!("SysListView32"),
            "",
            (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
            ID_PREINSTALL_LIST,
        )?;
        let _ = SendMessageW(
            package_list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_CHECKBOXES | LVS_EX_DOUBLEBUFFER | LVS_EX_INFOTIP) as isize),
        );
        insert_columns(package_list);
        let selected_ids = selected
            .iter()
            .map(|package| package.id.to_ascii_lowercase())
            .collect();
        let mut dialog = Self {
            shell,
            categories,
            selected_ids,
            active_category: 0,
            category_list,
            package_list,
            selection_write_in_progress: Cell::new(false),
        };
        dialog.populate_categories();
        dialog.apply_theme();
        dialog.render_active_category();
        dialog.layout();
        Ok(dialog)
    }

    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.package_list
    }

    pub fn owns_category_list(&self, control: HWND) -> bool {
        control == self.category_list
    }

    pub fn accepts_list_change(&self, control: HWND) -> bool {
        self.owns_list(control) && !self.selection_write_in_progress.get()
    }

    pub unsafe fn handle_category_changed(&mut self, selected: usize) {
        let Ok(selected) = isize::try_from(selected) else {
            return;
        };
        self.apply_category_selection(selected);
    }

    /// Reconciles the native ListView's current selection with the rendered category.
    ///
    /// `LVN_ITEMCHANGED` is the primary path. The existing modeless-dialog timer remains a narrow
    /// fallback for hosts that temporarily suppress a forwarded notification; it only rebuilds
    /// after the authoritative selected row actually changes.
    pub unsafe fn reconcile_category_selection(&mut self) {
        const LVM_GETNEXTITEM: u32 = 0x100C;
        const LVNI_SELECTED: isize = 0x0002;
        let selected = SendMessageW(
            self.category_list,
            LVM_GETNEXTITEM,
            WPARAM(usize::MAX),
            LPARAM(LVNI_SELECTED),
        )
        .0;
        self.apply_category_selection(selected);
    }

    unsafe fn apply_category_selection(&mut self, selected: isize) {
        let Some(selected) =
            changed_category_index(self.active_category, selected, self.categories.len())
        else {
            return;
        };
        self.sync_active_selection();
        self.active_category = selected;
        self.render_active_category();
    }

    pub unsafe fn handle_list_changed(&mut self) {
        if self.selection_write_in_progress.get() {
            return;
        }
        self.sync_active_selection();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<PreinstallDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Primary => {
                self.sync_active_selection();
                let packages = self.selected_packages();
                match validate_selected_packages(&packages) {
                    Ok(()) => Some(PreinstallDialogIntent::Apply(packages)),
                    Err(error) => {
                        self.shell.relocalize(
                            &crate::tr!("选择预装应用"),
                            &crate::tr!("选择预装应用"),
                            &crate::tr!("选择的软件数据无效：{}", error),
                            &crate::tr!("确定"),
                        );
                        self.shell.show_modeless();
                        None
                    }
                }
            }
            DialogResult::Secondary | DialogResult::Cancel => Some(PreinstallDialogIntent::Close),
        }
    }

    pub unsafe fn layout(&self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = rect.right.max(0);
        let height = rect.bottom.max(0);
        let category_width = scale(180, dpi).min((width / 3).max(scale(120, dpi)));
        let _ = MoveWindow(self.category_list, 0, 0, category_width, height, true);
        set_single_column_width(self.category_list);
        let package_x = category_width + metrics.control_gap;
        let package_width = (width - package_x).max(0);
        let _ = MoveWindow(self.package_list, package_x, 0, package_width, height, true);
        let checkbox_column_width = scale(36, dpi).min(package_width);
        let _ = SendMessageW(
            self.package_list,
            LVM_SETCOLUMNWIDTH,
            WPARAM(0),
            LPARAM(checkbox_column_width as isize),
        );
        let _ = SendMessageW(
            self.package_list,
            LVM_SETCOLUMNWIDTH,
            WPARAM(1),
            LPARAM((package_width - checkbox_column_width - scale(4, dpi)).max(0) as isize),
        );
    }

    unsafe fn populate_categories(&mut self) {
        for (row, category) in self.categories.iter().enumerate() {
            insert_text_item(self.category_list, row as i32, &category.name);
        }
        if !self.categories.is_empty() {
            set_selected_item(self.category_list, 0);
        }
        set_single_column_width(self.category_list);
    }

    unsafe fn render_active_category(&mut self) {
        self.selection_write_in_progress.set(true);
        let _ = SendMessageW(self.package_list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        if let Some(category) = self.categories.get(self.active_category) {
            for (row, software) in category.items.iter().enumerate() {
                let label = if software.description.trim().is_empty() {
                    software.name.clone()
                } else {
                    format!("{}  —  {}", software.name, software.description)
                };
                insert_list_item(self.package_list, row as i32);
                set_list_subitem(self.package_list, row as i32, 1, &label);
                set_item_checked(
                    self.package_list,
                    row,
                    self.selected_ids
                        .contains(&software.id.to_ascii_lowercase()),
                );
            }
        }
        self.selection_write_in_progress.set(false);
    }

    unsafe fn sync_active_selection(&mut self) {
        let Some(category) = self.categories.get(self.active_category) else {
            return;
        };
        for (row, software) in category.items.iter().enumerate() {
            let id = software.id.to_ascii_lowercase();
            if item_checked(self.package_list, row) {
                self.selected_ids.insert(id);
            } else {
                self.selected_ids.remove(&id);
            }
        }
    }

    fn selected_packages(&self) -> Vec<SelectedSoftwarePackage> {
        selected_packages_for_ids(&self.categories, &self.selected_ids)
    }

    unsafe fn apply_theme(&self) {
        let palette = Palette::system();
        let _ = apply_list_view_theme(self.category_list, palette);
        let _ = apply_list_view_theme(self.package_list, palette);
    }
}

fn changed_category_index(active: usize, selected: isize, category_count: usize) -> Option<usize> {
    let selected = usize::try_from(selected).ok()?;
    (selected < category_count && selected != active).then_some(selected)
}

fn selected_packages_for_ids(
    categories: &[SoftwareCategory],
    selected_ids: &BTreeSet<String>,
) -> Vec<SelectedSoftwarePackage> {
    categories
        .iter()
        .flat_map(|category| category.items.iter())
        .filter(|software| {
            !software.vm_tools && selected_ids.contains(&software.id.to_ascii_lowercase())
        })
        .filter_map(NativeDownloadController::selected_package)
        .collect()
}

unsafe fn insert_columns(list: HWND) {
    let application_title = crate::tr!("应用");
    for (index, (title, width)) in [("", 36), (application_title.as_str(), 524)]
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

unsafe fn insert_category_column(list: HWND) {
    let mut text = wide("");
    let mut column = LVCOLUMNW {
        mask: LVCF_TEXT | LVCF_WIDTH,
        cx: 160,
        pszText: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTCOLUMNW,
        WPARAM(0),
        LPARAM((&mut column as *mut LVCOLUMNW) as isize),
    );
}

unsafe fn insert_text_item(list: HWND, row: i32, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTITEMW,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn set_selected_item(list: HWND, row: usize) {
    use windows::Win32::UI::Controls::LVIS_SELECTED;

    let mut item = LVITEMW {
        stateMask: LVIS_SELECTED,
        state: LVIS_SELECTED,
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_SETITEMSTATE,
        WPARAM(row),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn set_single_column_width(list: HWND) {
    let mut rect = RECT::default();
    let _ = GetClientRect(list, &mut rect);
    let width = (rect.right - rect.left).max(0);
    let _ = SendMessageW(list, LVM_SETCOLUMNWIDTH, WPARAM(0), LPARAM(width as isize));
}

unsafe fn insert_list_item(list: HWND, row: i32) {
    let mut value = wide("");
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTITEMW,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn set_list_subitem(list: HWND, row: i32, column: i32, value: &str) {
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
        0x104C, // LVM_SETITEMW
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn set_item_checked(list: HWND, index: usize, checked: bool) {
    let mut item = LVITEMW {
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_STATEIMAGEMASK),
        state: LIST_VIEW_ITEM_STATE_FLAGS(if checked {
            CHECKED_STATE_IMAGE
        } else {
            UNCHECKED_STATE_IMAGE
        }),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_SETITEMSTATE,
        WPARAM(index),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn item_checked(list: HWND, index: usize) -> bool {
    let state = SendMessageW(
        list,
        LVM_GETITEMSTATE,
        WPARAM(index),
        LPARAM(LVIS_STATEIMAGEMASK as isize),
    )
    .0 as u32;
    state & LVIS_STATEIMAGEMASK == CHECKED_STATE_IMAGE
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::config::OnlineSoftware;

    fn software(id: &str, vm_tools: bool) -> OnlineSoftware {
        OnlineSoftware {
            id: id.to_owned(),
            name: id.to_owned(),
            description: String::new(),
            update_date: String::new(),
            file_size: String::new(),
            version: None,
            icon_url: None,
            download_url: format!("https://example.invalid/{id}.exe"),
            download_url_x86: None,
            download_url_nt5: None,
            filename: format!("{id}.exe"),
            silent_command: Some("\"{installer}\" /S".to_owned()),
            requires_admin: true,
            vm_tools,
            md5: None,
            sha256: None,
            md5_x86: None,
            sha256_x86: None,
            md5_nt5: None,
            sha256_nt5: None,
        }
    }

    #[test]
    fn vmware_tools_is_never_returned_by_generic_selection() {
        let categories = vec![SoftwareCategory {
            id: "tools".to_owned(),
            name: "Tools".to_owned(),
            description: String::new(),
            items: vec![software("ordinary", false), software("vmware", true)],
        }];
        let ids = ["ordinary".to_owned(), "vmware".to_owned()]
            .into_iter()
            .collect();
        let selected = selected_packages_for_ids(&categories, &ids);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "ordinary");
    }

    #[test]
    fn category_reconciliation_only_rebuilds_for_a_valid_changed_index() {
        assert_eq!(changed_category_index(0, 1, 3), Some(1));
        assert_eq!(changed_category_index(1, 1, 3), None);
        assert_eq!(changed_category_index(0, -1, 3), None);
        assert_eq!(changed_category_index(0, 3, 3), None);
    }
}
