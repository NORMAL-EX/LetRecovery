//! Dedicated native Inno-style UI for the legacy quick-partition editor.
//!
//! The dialog owns only presentation and the pure editor state. Disk inventory is supplied by the
//! host and every destructive action is returned as a fingerprinted intent. No DiskPart command,
//! resize operation, refresh enumeration, or other host I/O is performed here.

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, InvalidateRect, HFONT};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW, SetWindowTextW,
    ShowWindow, BM_SETCHECK, BS_AUTORADIOBUTTON, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOHSCROLL, SW_HIDE, SW_SHOW, WM_SETFONT,
    WS_BORDER, WS_TABSTOP,
};

use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    arrange_field, measure_text, measured_button_width, preferred_list_height, FieldArrangement,
    LayoutMetrics,
};
use super::super::theme::{apply_control_theme, apply_list_view_theme, NativeControlKind, Palette};
use crate::core::disk::PartitionStyle;
use crate::core::native_quick_partition::QuickPartitionRequest;
use crate::core::native_quick_partition_dialog::{
    EditorRow, ExistingPartitionResizeRequest, QuickPartitionDialogState,
};
use crate::core::quick_partition::{DiskPartitionInfo, PartitionLayout, PhysicalDisk};

pub const ID_DISK: u16 = 65_300;
pub const ID_STYLE_MBR: u16 = 65_301;
pub const ID_STYLE_GPT: u16 = 65_302;
pub const ID_ADD_PARTITION: u16 = 65_303;
pub const ID_ADD_ESP: u16 = 65_304;
pub const ID_DELETE: u16 = 65_305;
pub const ID_APPLY_SIZE: u16 = 65_306;
const ID_PARTITIONS: u16 = 65_307;
const ID_SIZE: u16 = 65_308;
const RADIO_CONTROL_KIND: NativeControlKind = NativeControlKind::General;

const LVM_SETITEMTEXTW_LOCAL: u32 = 0x104C;
const LVM_SETITEMSTATE: u32 = 0x102B;
const LVNI_SELECTED: isize = 0x0002;
const LVIS_SELECTED: u32 = 0x0002;

#[derive(Clone, Debug, PartialEq)]
pub enum QuickPartitionDialogIntent {
    RefreshInventory,
    RequestConfirmation(QuickPartitionRequest),
    RequestExistingResize(ExistingPartitionResizeRequest),
    Close,
}

#[derive(Clone, Copy)]
struct Controls {
    disk_label: HWND,
    disk: HWND,
    style_label: HWND,
    style_mbr: HWND,
    style_gpt: HWND,
    recommendation: HWND,
    add_partition: HWND,
    add_esp: HWND,
    delete: HWND,
    partitions: HWND,
    size_label: HWND,
    size: HWND,
    apply_size: HWND,
    warning: HWND,
    status: HWND,
}

pub struct NativeQuickPartitionDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: QuickPartitionDialogState,
    font: HFONT,
}

impl NativeQuickPartitionDialog {
    pub unsafe fn create(
        owner: HWND,
        recommended_style: PartitionStyle,
        used_drive_letters: Vec<char>,
        system_drive: char,
    ) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("一键分区"),
                title: crate::tr!("一键分区"),
                description: crate::tr!("选择物理磁盘并规划要创建的分区。"),
                width: 780,
                height: 650,
                buttons: DialogButtons {
                    primary: crate::tr!("一键分区"),
                    secondary: Some(crate::tr!("刷新")),
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
            state: QuickPartitionDialogState::new(
                recommended_style,
                used_drive_letters,
                system_drive,
            ),
            font,
        };
        dialog.apply_font_and_theme();
        dialog.layout();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &QuickPartitionDialogState {
        &self.state
    }

    pub fn owns_choice(&self, control: HWND) -> bool {
        control == self.controls.disk
    }

    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.controls.partitions
    }

    pub fn owns_command(command_id: u16) -> bool {
        matches!(
            command_id,
            ID_STYLE_MBR | ID_STYLE_GPT | ID_ADD_PARTITION | ID_ADD_ESP | ID_DELETE | ID_APPLY_SIZE
        )
    }

    pub unsafe fn set_loading(&mut self) {
        self.state.begin_refresh();
        self.render_state();
    }

    pub unsafe fn set_inventory(&mut self, result: Result<Vec<PhysicalDisk>, String>) {
        self.state.apply_inventory(result);
        self.render_state();
    }

    pub unsafe fn handle_choice_changed(&mut self, control: HWND) -> bool {
        if control != self.controls.disk {
            return false;
        }
        let index = SendMessageW(control, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let number = combo_inventory_index(index, self.state.disks.len())
            .and_then(|index| self.state.disks.get(index))
            .map(|disk| disk.disk_number);
        self.state.select_disk(number);
        self.render_state();
        true
    }

    pub unsafe fn handle_list_changed(&mut self) {
        let rows = self.state.rows().collect::<Vec<_>>();
        let index = SendMessageW(
            self.controls.partitions,
            LVM_GETNEXTITEM,
            WPARAM(usize::MAX),
            LPARAM(LVNI_SELECTED),
        )
        .0;
        self.state.select_row(
            (index >= 0)
                .then(|| rows.get(index as usize).copied())
                .flatten(),
        );
        self.render_selection();
    }

    pub unsafe fn handle_command(&mut self, command_id: u16) -> Option<QuickPartitionDialogIntent> {
        match command_id {
            ID_STYLE_MBR => self.state.set_partition_style(PartitionStyle::MBR),
            ID_STYLE_GPT => self.state.set_partition_style(PartitionStyle::GPT),
            ID_ADD_PARTITION => {
                self.state.add_data_partition();
            }
            ID_ADD_ESP => {
                self.state.add_esp_partition();
            }
            ID_DELETE => {
                self.state.delete_selected();
            }
            ID_APPLY_SIZE => {
                self.state.resize_size_text = window_text(self.controls.size);
                match self.state.apply_resize_text() {
                    Ok(Some(request)) => {
                        self.render_state();
                        return Some(QuickPartitionDialogIntent::RequestExistingResize(request));
                    }
                    Ok(None) => {}
                    Err(error) => self.state.message = error,
                }
            }
            _ => return None,
        }
        self.render_state();
        None
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
        // Reassert the shared Inno radio painter after the shell's final descendant theme pass;
        // USER32 still owns grouping, keyboard input and accessibility for both partition styles.
        self.apply_font_and_theme();
    }

    pub unsafe fn take_intent(&mut self) -> Option<QuickPartitionDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Secondary => {
                self.set_loading();
                Some(QuickPartitionDialogIntent::RefreshInventory)
            }
            DialogResult::Primary => match self.state.quick_partition_request() {
                Ok(request) => Some(QuickPartitionDialogIntent::RequestConfirmation(request)),
                Err(error) => {
                    self.state.message = error;
                    self.render_state();
                    self.shell.show_modeless();
                    self.apply_font_and_theme();
                    None
                }
            },
            DialogResult::Cancel => Some(QuickPartitionDialogIntent::Close),
        }
    }

    pub unsafe fn layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_height = metrics.label_height;
        let label_offset = ((metrics.field_height - label_height) / 2).max(0);
        let disk_label_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.disk_label),
            None,
        )
        .width;
        let disk_field = arrange_field(width, disk_label_width, scale(260, dpi), dpi);
        let mut y = 0;
        match disk_field {
            FieldArrangement::Inline {
                label_width,
                control_x,
                control_width,
            } => {
                move_control(
                    self.controls.disk_label,
                    0,
                    y + label_offset,
                    label_width,
                    label_height,
                );
                move_control(
                    self.controls.disk,
                    control_x,
                    y,
                    control_width,
                    scale(180, dpi),
                );
                y += metrics.field_height;
            }
            FieldArrangement::Stacked => {
                move_control(self.controls.disk_label, 0, y, width, label_height);
                y += label_height + metrics.tight_gap;
                move_control(self.controls.disk, 0, y, width, scale(180, dpi));
                y += metrics.field_height;
            }
        }
        y += metrics.control_gap;

        let style_label_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.style_label),
            None,
        )
        .width;
        let mbr_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("MBR"),
            dpi,
            scale(64, dpi),
        );
        let gpt_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("GPT"),
            dpi,
            scale(64, dpi),
        );
        let mut x = 0;
        move_control(
            self.controls.style_label,
            x,
            y + label_offset,
            style_label_width,
            label_height,
        );
        x += style_label_width + metrics.control_gap;
        move_control(
            self.controls.style_mbr,
            x,
            y,
            mbr_width,
            metrics.button_height,
        );
        x += mbr_width + metrics.control_gap;
        move_control(
            self.controls.style_gpt,
            x,
            y,
            gpt_width,
            metrics.button_height,
        );
        x += gpt_width + metrics.control_gap;
        let recommendation = window_text(self.controls.recommendation);
        let recommendation_width =
            measure_text(self.shell.hwnd(), self.font, &recommendation, None).width;
        if x + recommendation_width <= width {
            move_control(
                self.controls.recommendation,
                x,
                y + label_offset,
                width - x,
                label_height,
            );
            y += metrics.button_height;
        } else {
            y += metrics.button_height + metrics.tight_gap;
            move_control(self.controls.recommendation, 0, y, width, label_height);
            y += label_height;
        }
        y += metrics.control_gap;

        x = 0;
        for (control, visible) in [
            (self.controls.add_partition, true),
            (
                self.controls.add_esp,
                self.state.partition_style == PartitionStyle::GPT,
            ),
            (self.controls.delete, true),
        ] {
            if !visible {
                continue;
            }
            let button_width = measured_button_width(
                self.shell.hwnd(),
                self.font,
                &window_text(control),
                dpi,
                scale(75, dpi),
            );
            if x > 0 && x + button_width > width {
                x = 0;
                y += metrics.button_height + metrics.control_gap;
            }
            move_control(control, x, y, button_width, metrics.button_height);
            x += button_width + metrics.control_gap;
        }
        y += metrics.button_height + metrics.control_gap;

        let list_height = preferred_list_height(self.state.rows().count(), dpi, 3, 8);
        move_control(self.controls.partitions, 0, y, width, list_height);
        y += list_height + metrics.control_gap;

        let size_label_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.size_label),
            None,
        )
        .width;
        let size_width = scale(105, dpi);
        let apply_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.apply_size),
            dpi,
            scale(75, dpi),
        );
        move_control(
            self.controls.size_label,
            0,
            y + label_offset,
            size_label_width,
            label_height,
        );
        x = size_label_width + metrics.control_gap;
        move_control(self.controls.size, x, y, size_width, metrics.field_height);
        x += size_width + metrics.control_gap;
        move_control(
            self.controls.apply_size,
            x,
            y,
            apply_width,
            metrics.button_height,
        );
        x += apply_width + metrics.control_gap;
        let warning_text = window_text(self.controls.warning);
        let warning_width = measure_text(self.shell.hwnd(), self.font, &warning_text, None).width;
        if x + warning_width <= width {
            move_control(
                self.controls.warning,
                x,
                y + label_offset,
                width - x,
                label_height,
            );
            y += metrics.field_height;
        } else {
            y += metrics.field_height + metrics.tight_gap;
            let warning_height =
                measure_text(self.shell.hwnd(), self.font, &warning_text, Some(width))
                    .height
                    .max(label_height);
            move_control(self.controls.warning, 0, y, width, warning_height);
            y += warning_height;
        }
        let status_text = window_text(self.controls.status);
        if !status_text.is_empty() {
            y += metrics.control_gap;
            let status_height =
                measure_text(self.shell.hwnd(), self.font, &status_text, Some(width))
                    .height
                    .max(label_height);
            move_control(self.controls.status, 0, y, width, status_height);
            y += status_height;
        }
        self.shell.fit_content_height(logical_height(y, dpi));
        for (column, value) in partition_columns(width, dpi).into_iter().enumerate() {
            let _ = SendMessageW(
                self.controls.partitions,
                LVM_SETCOLUMNWIDTH,
                WPARAM(column),
                LPARAM(value as isize),
            );
        }
    }

    unsafe fn render_state(&mut self) {
        refill_disks(
            self.controls.disk,
            &self.state.disks,
            self.state.selected_disk_number,
        );
        set_radio(
            self.controls.style_mbr,
            self.state.partition_style == PartitionStyle::MBR,
        );
        set_radio(
            self.controls.style_gpt,
            self.state.partition_style == PartitionStyle::GPT,
        );
        set_text(
            self.controls.recommendation,
            &crate::tr!("推荐：{}", self.state.recommended_style.to_string()),
        );
        refill_partitions(self.controls.partitions, &self.state);
        self.render_selection();
        let has_disk = !self.state.loading && self.state.selected_disk().is_some();
        let _ = EnableWindow(self.controls.disk, !self.state.loading);
        for control in [
            self.controls.style_mbr,
            self.controls.style_gpt,
            self.controls.add_partition,
            self.controls.partitions,
        ] {
            let _ = EnableWindow(control, has_disk);
        }
        let _ = EnableWindow(
            self.controls.add_esp,
            has_disk && self.state.partition_style == PartitionStyle::GPT,
        );
        let _ = ShowWindow(
            self.controls.add_esp,
            if self.state.partition_style == PartitionStyle::GPT {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        self.shell
            .set_primary_enabled(self.state.quick_partition_request().is_ok());
    }

    unsafe fn render_selection(&self) {
        set_text(self.controls.size, &self.state.resize_size_text);
        let selected = self.state.selected_row.is_some();
        let _ = EnableWindow(self.controls.size, selected);
        let _ = EnableWindow(self.controls.apply_size, selected);
        // Disabled/enabled transitions can leave USER32's previous one-pixel bottom edge cached
        // until another input message.  Repaint only this owner-drawn button, without erasing the
        // row behind it, so all four Inno edges are present in the same frame.
        let _ = InvalidateRect(self.controls.apply_size, None, false);
        let _ = EnableWindow(
            self.controls.delete,
            matches!(self.state.selected_row, Some(EditorRow::Planned(_))),
        );
        set_text(self.controls.status, &self.state.message);
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        apply_control_theme(self.controls.disk, palette, NativeControlKind::Field);
        apply_control_theme(self.controls.size, palette, NativeControlKind::Field);
        apply_list_view_theme(self.controls.partitions, palette);
        for radio in [self.controls.style_mbr, self.controls.style_gpt] {
            apply_control_theme(radio, palette, RADIO_CONTROL_KIND);
        }
        for button in [
            self.controls.add_partition,
            self.controls.add_esp,
            self.controls.delete,
            self.controls.apply_size,
        ] {
            apply_control_theme(button, palette, NativeControlKind::General);
        }
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(
                self.controls.partitions,
                message,
                WPARAM(0),
                LPARAM(color.0 as isize),
            );
        }
    }

    fn controls(&self) -> [HWND; 15] {
        let c = self.controls;
        [
            c.disk_label,
            c.disk,
            c.style_label,
            c.style_mbr,
            c.style_gpt,
            c.recommendation,
            c.add_partition,
            c.add_esp,
            c.delete,
            c.partitions,
            c.size_label,
            c.size,
            c.apply_size,
            c.warning,
            c.status,
        ]
    }
}

impl Drop for NativeQuickPartitionDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<Controls> {
    let label = |text: &str| child(parent, w!("STATIC"), text, 0, 0);
    let button = |text: &str, id| {
        child(
            parent,
            w!("BUTTON"),
            text,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            id,
        )
    };
    let partitions = child(
        parent,
        w!("SysListView32"),
        "",
        (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
        ID_PARTITIONS,
    )?;
    let _ = SendMessageW(
        partitions,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
    );
    insert_columns(partitions);
    Ok(Controls {
        disk_label: label(&crate::tr!("选择磁盘:"))?,
        disk: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_DISK,
        )?,
        style_label: label(&crate::tr!("分区表类型:"))?,
        style_mbr: child(
            parent,
            w!("BUTTON"),
            "MBR",
            BS_AUTORADIOBUTTON | WS_TABSTOP.0 as i32,
            ID_STYLE_MBR,
        )?,
        style_gpt: child(
            parent,
            w!("BUTTON"),
            "GPT",
            BS_AUTORADIOBUTTON | WS_TABSTOP.0 as i32,
            ID_STYLE_GPT,
        )?,
        recommendation: label("")?,
        add_partition: button(&crate::tr!("添加分区"), ID_ADD_PARTITION)?,
        add_esp: button(&crate::tr!("创建 ESP 分区 (500 MB)"), ID_ADD_ESP)?,
        delete: button(&crate::tr!("删除"), ID_DELETE)?,
        partitions,
        size_label: label(&crate::tr!("新大小 (GB):"))?,
        size: child(
            parent,
            w!("EDIT"),
            "",
            ES_AUTOHSCROLL | WS_TABSTOP.0 as i32,
            ID_SIZE,
        )?,
        apply_size: button(&crate::tr!("调整大小"), ID_APPLY_SIZE)?,
        warning: label(&crate::tr!("提示: 一键分区会清除整个磁盘"))?,
        status: label("")?,
    })
}

unsafe fn insert_columns(list: HWND) {
    for (index, title) in ["状态", "分区卷", "大小", "已用/可用", "卷标", "文件系统"]
        .into_iter()
        .enumerate()
    {
        let mut text = wide(crate::tr!(title));
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: 100,
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

unsafe fn refill_disks(combo: HWND, disks: &[PhysicalDisk], selected: Option<u32>) {
    let _ = SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for disk in disks {
        add_combo_item(combo, &disk.display_name());
    }
    let index = selected
        .and_then(|number| disks.iter().position(|disk| disk.disk_number == number))
        .map_or(NO_COMBO_SELECTION, |index| index);
    let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
}

unsafe fn refill_partitions(list: HWND, state: &QuickPartitionDialogState) {
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    let mut row = 0;
    if let Some(disk) = state.selected_disk() {
        for partition in &disk.partitions {
            insert_row(list, row, existing_columns(partition));
            row += 1;
        }
    }
    for layout in &state.planned {
        insert_row(list, row, planned_columns(layout));
        row += 1;
    }
    if let Some(selected) = state.selected_row {
        let existing_count = state
            .selected_disk()
            .map_or(0, |disk| disk.partitions.len());
        let index = match selected {
            EditorRow::Existing(index) => index,
            EditorRow::Planned(index) => existing_count + index,
        };
        let mut item = LVITEMW {
            stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED),
            state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_SETITEMSTATE,
            WPARAM(index),
            LPARAM((&mut item as *mut LVITEMW) as isize),
        );
    }
}

unsafe fn insert_row(list: HWND, row: i32, columns: [String; 6]) {
    for (column, value) in columns.into_iter().enumerate() {
        let mut value = wide(value);
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: row,
            iSubItem: column as i32,
            pszText: PWSTR(value.as_mut_ptr()),
            ..Default::default()
        };
        let message = if column == 0 {
            LVM_INSERTITEMW
        } else {
            LVM_SETITEMTEXTW_LOCAL
        };
        let _ = SendMessageW(
            list,
            message,
            WPARAM(0),
            LPARAM((&mut item as *mut LVITEMW) as isize),
        );
    }
}

fn existing_columns(partition: &DiskPartitionInfo) -> [String; 6] {
    [
        crate::tr!("已有"),
        partition_name(
            partition.drive_letter,
            partition.is_esp,
            partition.is_msr,
            partition.is_recovery,
        ),
        format!("{:.1} GB", partition.size_gb()),
        format!("{:.1} / {:.1} GB", partition.used_gb(), partition.free_gb()),
        display_value(&partition.label),
        display_value(&partition.file_system),
    ]
}

fn planned_columns(layout: &PartitionLayout) -> [String; 6] {
    [
        crate::tr!("新建"),
        partition_name(layout.drive_letter, layout.is_esp, false, false),
        format!("{:.1} GB", layout.size_gb),
        format!("0.0 / {:.1} GB", layout.size_gb),
        display_value(&layout.label),
        display_value(&layout.file_system),
    ]
}

fn partition_name(letter: Option<char>, is_esp: bool, is_msr: bool, is_recovery: bool) -> String {
    if is_esp {
        "ESP".into()
    } else if is_msr {
        "MSR".into()
    } else if is_recovery {
        crate::tr!("恢复分区")
    } else {
        letter
            .map(|letter| format!("{letter}:"))
            .unwrap_or_else(|| crate::tr!("未分配盘符"))
    }
}

fn display_value(value: &str) -> String {
    if value.trim().is_empty() {
        "—".into()
    } else {
        value.into()
    }
}

fn partition_columns(width: i32, dpi: u32) -> [i32; 6] {
    let usable = (width - scale(4, dpi)).max(0);
    let status = usable * 11 / 100;
    let drive = usable * 15 / 100;
    let size = usable * 14 / 100;
    let usage = usable * 21 / 100;
    let fs = usable * 14 / 100;
    let label = usable - status - drive - size - usage - fs;
    [status, drive, size, usage, label, fs]
}

unsafe fn set_radio(control: HWND, checked: bool) {
    let _ = SendMessageW(
        control,
        BM_SETCHECK,
        WPARAM(usize::from(checked)),
        LPARAM(0),
    );
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

unsafe fn window_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let read = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..read])
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
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

    #[test]
    fn partition_style_radios_keep_native_general_theme_role() {
        assert_eq!(RADIO_CONTROL_KIND, NativeControlKind::General);
    }

    #[test]
    fn columns_remain_positive_and_use_the_full_width_at_supported_dpi() {
        for dpi in [96, 144, 192] {
            for logical_width in [420, 720] {
                let width = scale(logical_width, dpi);
                let columns = partition_columns(width, dpi);
                assert!(columns.into_iter().all(|column| column > 0));
                assert_eq!(columns.into_iter().sum::<i32>(), width - scale(4, dpi));
            }
        }
    }

    #[test]
    fn rows_distinguish_existing_and_planned_without_editing_existing_metadata() {
        let existing = DiskPartitionInfo {
            partition_number: 1,
            size_bytes: 10 * 1024 * 1024 * 1024,
            offset_bytes: 0,
            drive_letter: Some('D'),
            label: "Archive".into(),
            file_system: "NTFS".into(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            partition_type: String::new(),
            used_bytes: 2 * 1024 * 1024 * 1024,
            free_bytes: 8 * 1024 * 1024 * 1024,
            is_active: false,
        };
        assert_eq!(existing_columns(&existing)[0], crate::tr!("已有"));
        assert_eq!(existing_columns(&existing)[1], "D:");
        let planned = PartitionLayout {
            size_gb: 0.5,
            drive_letter: None,
            label: "EFI".into(),
            is_esp: true,
            file_system: "FAT32".into(),
        };
        assert_eq!(planned_columns(&planned)[0], crate::tr!("新建"));
        assert_eq!(planned_columns(&planned)[1], "ESP");
    }
}
