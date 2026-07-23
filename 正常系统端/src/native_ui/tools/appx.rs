//! Dedicated native dialog for the legacy APPX-removal toolbox workflow.
//!
//! The dialog exposes only an inventory-backed Windows target and an inventory-backed package
//! checklist. It never accepts a directory or deployment mode and never removes a package; all
//! host work is returned as typed intents for the existing APPX safety boundary.

use std::cell::Cell;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::Controls::{
    LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW,
    LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_CHECKBOXES,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, BN_CLICKED, BS_OWNERDRAW,
    CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL,
    SW_HIDE, SW_SHOW, WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    arrange_field, measure_text, measured_button_width, preferred_list_height, FieldArrangement,
    LayoutMetrics,
};
use super::super::theme::{apply_control_theme, apply_list_view_theme, NativeControlKind, Palette};
use crate::core::native_appx_selection::{
    AppxSelectionAction, NativeAppxDialogIntent, NativeAppxDialogState, NativeAppxSelectionError,
};
use crate::core::native_tool_inventory::InventoryEntry;

pub const ID_APPX_TARGET: u16 = 65_000;
pub const ID_APPX_SELECT_ALL: u16 = 65_001;
pub const ID_APPX_SELECT_NONE: u16 = 65_002;
pub const ID_APPX_INVERT: u16 = 65_003;
pub const ID_APPX_REFRESH: u16 = 65_004;
const ID_APPX_LIST: u16 = 65_005;
const ID_APPX_COUNT: u16 = 65_006;
const ID_APPX_STATUS: u16 = 65_007;
const ID_APPX_TARGET_LABEL: u16 = 65_008;

const LVM_SETITEMSTATE: u32 = 0x102B;
const LVM_GETITEMSTATE: u32 = 0x102C;
const LVIS_STATEIMAGEMASK: u32 = 0xF000;
const CHECKED_STATE_IMAGE: u32 = 2 << 12;
const UNCHECKED_STATE_IMAGE: u32 = 1 << 12;

#[derive(Clone, Copy)]
struct AppxControls {
    target_label: HWND,
    target: HWND,
    select_all: HWND,
    select_none: HWND,
    invert: HWND,
    refresh: HWND,
    count: HWND,
    packages: HWND,
    status: HWND,
}

pub struct NativeAppxDialog {
    pub shell: DialogShell,
    controls: AppxControls,
    state: NativeAppxDialogState,
    font: HFONT,
    selection_write_in_progress: Cell<bool>,
}

impl NativeAppxDialog {
    pub unsafe fn create(owner: HWND, state: NativeAppxDialogState) -> windows::core::Result<Self> {
        let description = if state.is_pe_environment {
            crate::tr!("移除离线系统中预装的 Microsoft Store 应用")
        } else {
            crate::tr!("移除当前系统或离线系统中的 Microsoft Store 应用")
        };
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("移除APPX应用"),
                title: crate::tr!("移除APPX应用"),
                description,
                width: 720,
                height: 560,
                buttons: DialogButtons {
                    primary: crate::tr!("移除选中应用"),
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
        let controls = create_controls(shell.content())?;
        let mut dialog = Self {
            shell,
            controls,
            state,
            font,
            selection_write_in_progress: Cell::new(false),
        };
        dialog.apply_font_and_theme();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &NativeAppxDialogState {
        &self.state
    }

    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.controls.packages
    }

    pub fn owns_command(command_id: u16) -> bool {
        matches!(
            command_id,
            ID_APPX_TARGET
                | ID_APPX_SELECT_ALL
                | ID_APPX_SELECT_NONE
                | ID_APPX_INVERT
                | ID_APPX_REFRESH
        )
    }

    /// Stock buttons reuse their command id for focus notifications. Only a real click may mutate
    /// the package selection, and the target ComboBox only reacts to a real selection change.
    pub fn accepts_command(command_id: u16, notification: u16) -> bool {
        match command_id {
            ID_APPX_TARGET => notification == CBN_SELCHANGE as u16,
            ID_APPX_SELECT_ALL | ID_APPX_SELECT_NONE | ID_APPX_INVERT | ID_APPX_REFRESH => {
                notification == BN_CLICKED as u16
            }
            _ => false,
        }
    }

    pub fn accepts_list_change(&self, control: HWND) -> bool {
        self.owns_list(control)
            && list_change_is_user_driven(self.selection_write_in_progress.get())
    }

    /// Handles a forwarded target/selection/refresh command and returns host work, if any.
    pub unsafe fn handle_command(
        &mut self,
        command_id: u16,
    ) -> Result<Option<NativeAppxDialogIntent>, NativeAppxSelectionError> {
        match command_id {
            ID_APPX_TARGET => {
                let selected = self.selected_target_from_control();
                if selected == self.state.selected_target {
                    return Ok(None);
                }
                let intent = self.state.select_target(selected)?;
                self.render_state();
                Ok(intent)
            }
            ID_APPX_SELECT_ALL => {
                self.sync_selection_from_list();
                self.state.apply_selection(AppxSelectionAction::SelectAll);
                self.render_selection();
                Ok(None)
            }
            ID_APPX_SELECT_NONE => {
                self.state.apply_selection(AppxSelectionAction::SelectNone);
                self.render_selection();
                Ok(None)
            }
            ID_APPX_INVERT => {
                self.sync_selection_from_list();
                self.state.apply_selection(AppxSelectionAction::Invert);
                self.render_selection();
                Ok(None)
            }
            ID_APPX_REFRESH => {
                let intent = self.state.refresh_intent()?;
                self.render_state();
                Ok(Some(intent))
            }
            _ => Ok(None),
        }
    }

    pub unsafe fn handle_list_changed(&mut self) {
        if !list_change_is_user_driven(self.selection_write_in_progress.get()) {
            return;
        }
        self.sync_selection_from_list();
        self.render_selection_summary();
    }

    pub unsafe fn set_targets(
        &mut self,
        result: Result<Vec<InventoryEntry>, String>,
    ) -> Option<NativeAppxDialogIntent> {
        let mut load = None;
        match result {
            Ok(targets) => {
                self.state.set_targets(targets);
                if self.state.targets.is_empty() {
                    self.state.status = crate::tr!("未找到可用的 Windows 目标");
                } else if let Some(target) = self.state.selected_target.clone() {
                    load = self.state.select_target(Some(target)).ok().flatten();
                }
            }
            Err(error) => {
                self.state.set_targets(Vec::new());
                self.state.status = crate::tr!("加载目标系统失败：{}", error);
            }
        }
        self.render_state();
        load
    }

    pub unsafe fn set_targets_loading(&mut self) {
        self.state.set_targets(Vec::new());
        self.state.targets_loading = true;
        self.state.status = crate::tr!("正在检测Windows分区...");
        self.render_state();
    }

    pub unsafe fn set_packages(
        &mut self,
        inventory_target: &str,
        result: Result<Vec<InventoryEntry>, String>,
    ) -> bool {
        let applied = self.state.apply_package_inventory(inventory_target, result);
        if applied {
            self.render_state();
        }
        applied
    }

    pub unsafe fn set_status(&mut self, message: impl Into<String>) {
        self.state.status = message.into();
        self.render_selection_summary();
        let _ = ShowWindow(self.controls.status, SW_SHOW);
        self.fit_and_layout();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<NativeAppxDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Primary => {
                self.sync_selection_from_list();
                match self.state.removal_intent() {
                    Ok(intent) => Some(intent),
                    Err(error) => {
                        self.state.status = error.to_string();
                        self.render_selection_summary();
                        let _ = ShowWindow(self.controls.status, SW_SHOW);
                        self.fit_and_layout();
                        self.shell.show_modeless();
                        None
                    }
                }
            }
            DialogResult::Secondary | DialogResult::Cancel => Some(NativeAppxDialogIntent::Close),
        }
    }

    pub unsafe fn layout(&self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let height = (rect.bottom - rect.top).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label = crate::tr!("目标系统:");
        let label_width = measure_text(self.shell.hwnd(), self.font, &label, None).width;
        let field_y = 0;
        let combo_drop_height = metrics.field_height + metrics.list_row_height * 8;
        let mut y = match arrange_field(width, label_width, scale(240, dpi), dpi) {
            FieldArrangement::Inline {
                label_width,
                control_x,
                control_width,
            } => {
                let _ = MoveWindow(
                    self.controls.target_label,
                    0,
                    field_y + (metrics.field_height - metrics.label_height) / 2,
                    label_width,
                    metrics.label_height,
                    true,
                );
                let _ = MoveWindow(
                    self.controls.target,
                    control_x,
                    field_y,
                    control_width,
                    combo_drop_height,
                    true,
                );
                field_y + metrics.field_height
            }
            FieldArrangement::Stacked => {
                let _ = MoveWindow(
                    self.controls.target_label,
                    0,
                    field_y,
                    width,
                    metrics.label_height,
                    true,
                );
                let combo_y = field_y + metrics.label_height + metrics.tight_gap;
                let _ = MoveWindow(
                    self.controls.target,
                    0,
                    combo_y,
                    width,
                    combo_drop_height,
                    true,
                );
                combo_y + metrics.field_height
            }
        };

        y += metrics.section_gap;
        let button_specs = [
            (self.controls.select_all, crate::tr!("全选")),
            (self.controls.select_none, crate::tr!("全不选")),
            (self.controls.invert, crate::tr!("反选")),
            (self.controls.refresh, crate::tr!("刷新列表")),
        ];
        let button_widths: [i32; 4] = std::array::from_fn(|index| {
            measured_button_width(
                self.shell.hwnd(),
                self.font,
                &button_specs[index].1,
                dpi,
                scale(75, dpi),
            )
        });
        let mut button_x = 0;
        let mut button_y = y;
        for ((control, _), button_width) in button_specs.into_iter().zip(button_widths) {
            if button_x > 0 && button_x + button_width > width {
                button_x = 0;
                button_y += metrics.button_height + metrics.tight_gap;
            }
            let _ = MoveWindow(
                control,
                button_x,
                button_y,
                button_width,
                metrics.button_height,
                true,
            );
            button_x += button_width + metrics.control_gap;
        }
        let count_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("已选择 {} 个应用", self.state.selected_count()),
            None,
        )
        .width;
        let count_inline = width - button_x >= count_width + metrics.control_gap;
        let count_x = if count_inline { button_x } else { 0 };
        let count_y = if count_inline {
            button_y + (metrics.button_height - metrics.label_height) / 2
        } else {
            button_y + metrics.button_height + metrics.tight_gap
        };
        let _ = MoveWindow(
            self.controls.count,
            count_x,
            count_y,
            (width - count_x).max(count_width),
            metrics.label_height,
            true,
        );
        y = if count_inline {
            button_y + metrics.button_height
        } else {
            count_y + metrics.label_height
        };
        let list_y = y + metrics.control_gap;
        let status_visible = !self.state.status.is_empty();
        let status_height = if status_visible {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &self.state.status,
                Some(width),
            )
            .height
            .max(metrics.label_height)
        } else {
            0
        };
        let trailing_height = if status_visible {
            metrics.control_gap + status_height
        } else {
            0
        };
        let minimum_list = preferred_list_height(self.state.packages.len(), dpi, 3, 8);
        let list_height = (height - list_y - trailing_height).max(minimum_list);
        let _ = MoveWindow(self.controls.packages, 0, list_y, width, list_height, true);
        let _ = MoveWindow(
            self.controls.status,
            0,
            list_y + list_height + metrics.control_gap,
            width,
            status_height,
            true,
        );
        let _ = SendMessageW(
            self.controls.packages,
            LVM_SETCOLUMNWIDTH,
            WPARAM(0),
            LPARAM((width - scale(4, dpi)).max(0) as isize),
        );
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(scale(320, dpi));
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_width =
            measure_text(self.shell.hwnd(), self.font, &crate::tr!("目标系统:"), None).width;
        let field_height = match arrange_field(width, label_width, scale(240, dpi), dpi) {
            FieldArrangement::Inline { .. } => metrics.field_height,
            FieldArrangement::Stacked => {
                metrics.label_height + metrics.tight_gap + metrics.field_height
            }
        };
        let button_texts = [
            crate::tr!("全选"),
            crate::tr!("全不选"),
            crate::tr!("反选"),
            crate::tr!("刷新列表"),
        ];
        let button_widths = button_texts
            .into_iter()
            .map(|text| {
                measured_button_width(self.shell.hwnd(), self.font, &text, dpi, scale(75, dpi))
            })
            .collect::<Vec<_>>();
        let count_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("已选择 {} 个应用", self.state.selected_count()),
            None,
        )
        .width;
        let toolbar_height = toolbar_height(width, &button_widths, count_width, metrics);
        let list_height = preferred_list_height(self.state.packages.len(), dpi, 3, 8);
        let status_height = if self.state.status.is_empty() {
            0
        } else {
            metrics.control_gap
                + measure_text(
                    self.shell.hwnd(),
                    self.font,
                    &self.state.status,
                    Some(width),
                )
                .height
                .max(metrics.label_height)
        };
        let content_height = field_height
            + metrics.section_gap
            + toolbar_height
            + metrics.control_gap
            + list_height
            + status_height;
        self.shell
            .fit_content_height(pixels_to_logical(content_height, dpi));
        self.layout();
    }

    unsafe fn selected_target_from_control(&self) -> Option<String> {
        let selected = SendMessageW(self.controls.target, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let index = combo_inventory_index(selected, self.state.targets.len())?;
        self.state
            .targets
            .get(index)
            .map(|entry| entry.value.clone())
    }

    unsafe fn render_state(&mut self) {
        self.selection_write_in_progress.set(true);
        self.render_targets();
        let _ = SendMessageW(
            self.controls.packages,
            LVM_DELETEALLITEMS,
            WPARAM(0),
            LPARAM(0),
        );
        for (row, package) in self.state.packages.iter().enumerate() {
            insert_list_item(
                self.controls.packages,
                row as i32,
                if package.label.trim().is_empty() {
                    &package.value
                } else {
                    &package.label
                },
            );
        }
        self.render_selection_items();
        self.selection_write_in_progress.set(false);
        self.render_selection_summary();
        let packages_enabled = !self.state.packages_loading && !self.state.packages.is_empty();
        for control in [
            self.controls.select_all,
            self.controls.select_none,
            self.controls.invert,
            self.controls.packages,
        ] {
            let _ = EnableWindow(control, packages_enabled);
        }
        let _ = EnableWindow(self.controls.target, !self.state.targets_loading);
        let _ = EnableWindow(
            self.controls.refresh,
            !self.state.packages_loading && self.state.selected_target.is_some(),
        );
        let _ = ShowWindow(
            self.controls.status,
            if self.state.status.is_empty() {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        self.fit_and_layout();
    }

    unsafe fn render_targets(&self) {
        let _ = SendMessageW(self.controls.target, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        let mut selected = NO_COMBO_SELECTION;
        for (index, entry) in self.state.targets.iter().enumerate() {
            add_combo_item(self.controls.target, &entry.label);
            if self
                .state
                .selected_target
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&entry.value))
            {
                selected = index;
            }
        }
        let _ = SendMessageW(
            self.controls.target,
            CB_SETCURSEL,
            WPARAM(selected),
            LPARAM(0),
        );
    }

    unsafe fn render_selection(&self) {
        self.selection_write_in_progress.set(true);
        self.render_selection_items();
        self.selection_write_in_progress.set(false);
        self.render_selection_summary();
    }

    unsafe fn render_selection_items(&self) {
        for (index, package) in self.state.packages.iter().enumerate() {
            set_item_checked(
                self.controls.packages,
                index,
                self.state.is_selected(&package.value),
            );
        }
    }

    unsafe fn render_selection_summary(&self) {
        set_text(
            self.controls.count,
            &crate::tr!("已选择 {} 个应用", self.state.selected_count()),
        );
        set_text(self.controls.status, &self.state.status);
        self.shell
            .set_primary_enabled(self.state.removal_intent().is_ok());
    }

    unsafe fn sync_selection_from_list(&mut self) {
        for (index, package) in self.state.packages.clone().into_iter().enumerate() {
            self.state
                .set_package_selected(&package.value, item_checked(self.controls.packages, index));
        }
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        let _ = apply_list_view_theme(self.controls.packages, palette);
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(
                self.controls.packages,
                message,
                WPARAM(0),
                LPARAM(color.0 as isize),
            );
        }
        apply_control_theme(self.controls.target, palette, NativeControlKind::Field);
        for button in [
            self.controls.select_all,
            self.controls.select_none,
            self.controls.invert,
            self.controls.refresh,
        ] {
            apply_control_theme(button, palette, NativeControlKind::General);
        }
    }

    fn controls(&self) -> [HWND; 9] {
        let c = self.controls;
        [
            c.target_label,
            c.target,
            c.select_all,
            c.select_none,
            c.invert,
            c.refresh,
            c.count,
            c.packages,
            c.status,
        ]
    }
}

impl Drop for NativeAppxDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<AppxControls> {
    let button = |text: &str, id: u16| {
        child(
            parent,
            w!("BUTTON"),
            text,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            id,
        )
    };
    let packages = child(
        parent,
        w!("SysListView32"),
        "",
        (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
        ID_APPX_LIST,
    )?;
    let _ = SendMessageW(
        packages,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM(
            (LVS_EX_CHECKBOXES | LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP)
                as isize,
        ),
    );
    insert_column(packages);
    Ok(AppxControls {
        target_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("目标系统:"),
            0,
            ID_APPX_TARGET_LABEL,
        )?,
        target: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_APPX_TARGET,
        )?,
        select_all: button(&crate::tr!("全选"), ID_APPX_SELECT_ALL)?,
        select_none: button(&crate::tr!("全不选"), ID_APPX_SELECT_NONE)?,
        invert: button(&crate::tr!("反选"), ID_APPX_INVERT)?,
        refresh: button(&crate::tr!("刷新列表"), ID_APPX_REFRESH)?,
        count: child(parent, w!("STATIC"), "", 0, ID_APPX_COUNT)?,
        packages,
        status: child(parent, w!("STATIC"), "", 0, ID_APPX_STATUS)?,
    })
}

unsafe fn insert_column(list: HWND) {
    let mut text = wide(crate::tr!("应用"));
    let mut column = LVCOLUMNW {
        mask: LVCF_TEXT | LVCF_WIDTH,
        cx: 600,
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

unsafe fn insert_list_item(list: HWND, row: i32, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        iSubItem: 0,
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

unsafe fn add_combo_item(combo: HWND, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(
        combo,
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

fn pixels_to_logical(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) - 1) / i64::from(dpi.max(1))) as i32
}

fn toolbar_height(
    width: i32,
    button_widths: &[i32],
    count_width: i32,
    metrics: LayoutMetrics,
) -> i32 {
    let mut x = 0;
    let mut rows = 1;
    for button_width in button_widths {
        if x > 0 && x + button_width > width {
            x = 0;
            rows += 1;
        }
        x += button_width + metrics.control_gap;
    }
    let buttons_height = rows * metrics.button_height + (rows - 1) * metrics.tight_gap;
    if width - x >= count_width + metrics.control_gap {
        buttons_height
    } else {
        buttons_height + metrics.tight_gap + metrics.label_height
    }
}

fn list_change_is_user_driven(selection_write_in_progress: bool) -> bool {
    !selection_write_in_progress
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appx_dialog_has_only_inventory_and_selection_commands() {
        assert!(NativeAppxDialog::owns_command(ID_APPX_TARGET));
        assert!(NativeAppxDialog::owns_command(ID_APPX_SELECT_ALL));
        assert!(NativeAppxDialog::owns_command(ID_APPX_SELECT_NONE));
        assert!(NativeAppxDialog::owns_command(ID_APPX_INVERT));
        assert!(NativeAppxDialog::owns_command(ID_APPX_REFRESH));
    }

    #[test]
    fn appx_commands_ignore_focus_and_unrelated_control_notifications() {
        assert!(NativeAppxDialog::accepts_command(
            ID_APPX_TARGET,
            CBN_SELCHANGE as u16
        ));
        assert!(!NativeAppxDialog::accepts_command(
            ID_APPX_TARGET,
            BN_CLICKED as u16
        ));
        for command in [
            ID_APPX_SELECT_ALL,
            ID_APPX_SELECT_NONE,
            ID_APPX_INVERT,
            ID_APPX_REFRESH,
        ] {
            assert!(NativeAppxDialog::accepts_command(
                command,
                BN_CLICKED as u16
            ));
            assert!(!NativeAppxDialog::accepts_command(command, 6));
        }
    }

    #[test]
    fn programmatic_checkbox_updates_do_not_feed_partial_state_back_into_inventory() {
        assert!(!list_change_is_user_driven(true));
        assert!(list_change_is_user_driven(false));
    }

    #[test]
    fn control_ids_are_unique() {
        let ids = [
            ID_APPX_TARGET,
            ID_APPX_SELECT_ALL,
            ID_APPX_SELECT_NONE,
            ID_APPX_INVERT,
            ID_APPX_REFRESH,
            ID_APPX_LIST,
            ID_APPX_COUNT,
            ID_APPX_STATUS,
            ID_APPX_TARGET_LABEL,
        ];
        assert_eq!(
            ids.into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            ids.len()
        );
    }
}
