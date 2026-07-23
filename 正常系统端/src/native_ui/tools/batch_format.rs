//! Dedicated native dialog for the legacy batch-format toolbox entry.
//!
//! This module only presents a caller-supplied inventory of already-filtered safe fixed volumes
//! and returns the existing typed [`MutatingToolIntent::BatchFormat`] intent. It never enumerates
//! disks, validates against the host again, starts `format.com`, or formats a volume. The host must
//! refresh the inventory through the existing read-only boundary, show its destructive-operation
//! confirmation, and pass the intent through the existing typed executor.

use std::collections::BTreeSet;

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
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, BS_OWNERDRAW, SW_HIDE,
    SW_SHOW, WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use super::super::controls::{child, wide};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    measure_text, measured_button_width, preferred_list_height, LayoutMetrics,
};
use super::super::theme::{apply_control_theme, apply_list_view_theme, NativeControlKind, Palette};
use super::super::tool_dialogs_mutating::MutatingToolIntent;

pub const ID_SELECT_ALL: u16 = 64_700;
pub const ID_SELECT_NONE: u16 = 64_701;
pub const ID_INVERT_SELECTION: u16 = 64_702;
const ID_VOLUME_LIST: u16 = 64_703;
const ID_SELECTION_STATUS: u16 = 64_704;
const ID_LOAD_STATUS: u16 = 64_705;

const LVM_SETITEMTEXTW: u32 = 0x104C;
const LVM_SETITEMSTATE: u32 = 0x102B;
const LVM_GETITEMSTATE: u32 = 0x102C;
const LVIS_STATEIMAGEMASK: u32 = 0xF000;
const CHECKED_STATE_IMAGE: u32 = 2 << 12;
const UNCHECKED_STATE_IMAGE: u32 = 1 << 12;

const FIXED_FILE_SYSTEM: &str = "NTFS";
const FIXED_VOLUME_LABEL: &str = "新加卷";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchFormatVolume {
    pub drive: String,
    pub label: String,
    pub file_system: String,
    pub total_size_mb: u64,
    pub free_size_mb: u64,
}

impl BatchFormatVolume {
    pub fn new(
        drive: impl Into<String>,
        label: impl Into<String>,
        file_system: impl Into<String>,
        total_size_mb: u64,
        free_size_mb: u64,
    ) -> Self {
        Self {
            drive: drive.into(),
            label: label.into(),
            file_system: file_system.into(),
            total_size_mb,
            free_size_mb,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchFormatDialogState {
    pub loading: bool,
    pub volumes: Vec<BatchFormatVolume>,
    selected: BTreeSet<String>,
    pub message: String,
}

impl Default for BatchFormatDialogState {
    fn default() -> Self {
        Self {
            loading: true,
            volumes: Vec::new(),
            selected: BTreeSet::new(),
            message: crate::tr!("正在检测分区..."),
        }
    }
}

impl BatchFormatDialogState {
    pub fn selected_drives(&self) -> Vec<String> {
        self.volumes
            .iter()
            .filter(|volume| self.selected.contains(&volume.drive))
            .map(|volume| volume.drive.clone())
            .collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn is_selected(&self, drive: &str) -> bool {
        self.selected.contains(drive)
    }

    pub fn set_selected(&mut self, drive: &str, selected: bool) {
        if !self.volumes.iter().any(|volume| volume.drive == drive) {
            return;
        }
        if selected {
            self.selected.insert(drive.to_owned());
        } else {
            self.selected.remove(drive);
        }
    }

    pub fn select_all(&mut self) {
        self.selected = self
            .volumes
            .iter()
            .map(|volume| volume.drive.clone())
            .collect();
    }

    pub fn select_none(&mut self) {
        self.selected.clear();
    }

    pub fn invert_selection(&mut self) {
        self.selected = self
            .volumes
            .iter()
            .filter(|volume| !self.selected.contains(&volume.drive))
            .map(|volume| volume.drive.clone())
            .collect();
    }

    pub fn begin_refresh(&mut self) {
        self.loading = true;
        self.volumes.clear();
        self.selected.clear();
        self.message = crate::tr!("正在检测分区...");
    }

    pub fn apply_inventory(&mut self, result: Result<Vec<BatchFormatVolume>, String>) {
        self.loading = false;
        match result {
            Ok(volumes) => {
                self.volumes = sanitize_inventory(volumes);
                self.selected
                    .retain(|drive| self.volumes.iter().any(|volume| &volume.drive == drive));
                self.message = if self.volumes.is_empty() {
                    crate::tr!("未找到可格式化的分区")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.volumes.clear();
                self.selected.clear();
                self.message = crate::tr!("加载失败：{}", error);
            }
        }
    }

    pub fn execution_intent(&self) -> Option<MutatingToolIntent> {
        let partitions = self.selected_drives();
        (!self.loading && !partitions.is_empty()).then(|| MutatingToolIntent::BatchFormat {
            partitions,
            file_system: FIXED_FILE_SYSTEM.to_owned(),
            volume_label: FIXED_VOLUME_LABEL.to_owned(),
        })
    }
}

fn sanitize_inventory(volumes: Vec<BatchFormatVolume>) -> Vec<BatchFormatVolume> {
    let mut seen = BTreeSet::new();
    volumes
        .into_iter()
        .filter_map(|mut volume| {
            volume.drive = normalize_drive(&volume.drive)?;
            let protected = matches!(volume.drive.as_str(), "C:" | "X:");
            (!protected && seen.insert(volume.drive.clone())).then_some(volume)
        })
        .collect()
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

#[derive(Clone, Debug, PartialEq)]
pub enum BatchFormatDialogIntent {
    Refresh,
    RequestConfirmation(MutatingToolIntent),
    Close,
}

#[derive(Clone, Copy)]
struct BatchFormatControls {
    select_all: HWND,
    select_none: HWND,
    invert: HWND,
    selection_status: HWND,
    volumes: HWND,
    load_status: HWND,
}

pub struct NativeBatchFormatDialog {
    pub shell: DialogShell,
    controls: BatchFormatControls,
    state: BatchFormatDialogState,
    font: HFONT,
}

impl NativeBatchFormatDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("批量格式化"),
                title: crate::tr!("批量格式化"),
                description: crate::tr!("选择要格式化的分区（系统盘已自动隐藏）"),
                width: 680,
                height: 520,
                buttons: DialogButtons {
                    primary: crate::tr!("应用（格式化选中分区）"),
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
            state: BatchFormatDialogState::default(),
            font,
        };
        dialog.apply_font_and_theme();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &BatchFormatDialogState {
        &self.state
    }

    pub fn owns_command(command_id: u16) -> bool {
        matches!(
            command_id,
            ID_SELECT_ALL | ID_SELECT_NONE | ID_INVERT_SELECTION
        )
    }

    /// Lets the host identify `WM_NOTIFY` messages from this dialog's checkbox ListView.
    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.controls.volumes
    }

    /// Handle one of the three in-content selection buttons forwarded by [`DialogShell`].
    pub unsafe fn handle_command(&mut self, command_id: u16) -> bool {
        self.sync_selection_from_list();
        match command_id {
            ID_SELECT_ALL => self.state.select_all(),
            ID_SELECT_NONE => self.state.select_none(),
            ID_INVERT_SELECTION => self.state.invert_selection(),
            _ => return false,
        }
        self.render_selection();
        true
    }

    /// Refresh enablement after a checkbox click reported through `WM_NOTIFY` by the host.
    pub unsafe fn handle_list_changed(&mut self) {
        self.sync_selection_from_list();
        self.render_selection_summary();
    }

    pub unsafe fn set_inventory(&mut self, result: Result<Vec<BatchFormatVolume>, String>) {
        self.state.apply_inventory(result);
        self.render_state();
    }

    pub unsafe fn set_loading(&mut self) {
        self.state.begin_refresh();
        self.render_state();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<BatchFormatDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Secondary => {
                self.set_loading();
                Some(BatchFormatDialogIntent::Refresh)
            }
            DialogResult::Primary => {
                self.sync_selection_from_list();
                match self.state.execution_intent() {
                    Some(intent) => Some(BatchFormatDialogIntent::RequestConfirmation(intent)),
                    None => {
                        self.state.message = crate::tr!("请至少选择一个分区。");
                        self.render_selection_summary();
                        let _ = ShowWindow(self.controls.load_status, SW_SHOW);
                        self.fit_and_layout();
                        None
                    }
                }
            }
            DialogResult::Cancel => Some(BatchFormatDialogIntent::Close),
        }
    }

    pub unsafe fn layout(&self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let height = (rect.bottom - rect.top).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let buttons = [
            (self.controls.select_all, crate::tr!("全选")),
            (self.controls.select_none, crate::tr!("全不选")),
            (self.controls.invert, crate::tr!("反选")),
        ];
        let button_widths: [i32; 3] = std::array::from_fn(|index| {
            measured_button_width(
                self.shell.hwnd(),
                self.font,
                &buttons[index].1,
                dpi,
                scale(75, dpi),
            )
        });
        let mut x = 0;
        let mut toolbar_y = 0;
        for ((control, _), button_width) in buttons.into_iter().zip(button_widths) {
            if x > 0 && x + button_width > width {
                x = 0;
                toolbar_y += metrics.button_height + metrics.tight_gap;
            }
            let _ = MoveWindow(
                control,
                x,
                toolbar_y,
                button_width,
                metrics.button_height,
                true,
            );
            x += button_width + metrics.control_gap;
        }
        let summary = crate::tr!("已选择 {} 个分区", self.state.selected_count());
        let summary_width = measure_text(self.shell.hwnd(), self.font, &summary, None).width;
        let summary_inline = width - x >= summary_width + metrics.control_gap;
        let summary_x = if summary_inline { x } else { 0 };
        let summary_y = if summary_inline {
            toolbar_y + (metrics.button_height - metrics.label_height) / 2
        } else {
            toolbar_y + metrics.button_height + metrics.tight_gap
        };
        let _ = MoveWindow(
            self.controls.selection_status,
            summary_x,
            summary_y,
            (width - summary_x).max(summary_width),
            metrics.label_height,
            true,
        );
        let toolbar_bottom = if summary_inline {
            toolbar_y + metrics.button_height
        } else {
            summary_y + metrics.label_height
        };
        let list_y = toolbar_bottom + metrics.control_gap;
        let status_height = if self.state.message.is_empty() {
            0
        } else {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &self.state.message,
                Some(width),
            )
            .height
            .max(metrics.label_height)
        };
        let trailing_height = if status_height > 0 {
            metrics.control_gap + status_height
        } else {
            0
        };
        let minimum_list = preferred_list_height(self.state.volumes.len(), dpi, 3, 8);
        let list_height = (height - list_y - trailing_height).max(minimum_list);
        let _ = MoveWindow(self.controls.volumes, 0, list_y, width, list_height, true);
        let _ = MoveWindow(
            self.controls.load_status,
            0,
            list_y + list_height + metrics.control_gap,
            width,
            status_height,
            true,
        );
        for (index, column_width) in batch_format_column_widths(width, dpi)
            .into_iter()
            .enumerate()
        {
            let _ = SendMessageW(
                self.controls.volumes,
                LVM_SETCOLUMNWIDTH,
                WPARAM(index),
                LPARAM(column_width as isize),
            );
        }
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(scale(320, dpi));
        let metrics = LayoutMetrics::for_dpi(dpi);
        let button_widths =
            [crate::tr!("全选"), crate::tr!("全不选"), crate::tr!("反选")].map(|text| {
                measured_button_width(self.shell.hwnd(), self.font, &text, dpi, scale(75, dpi))
            });
        let summary_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("已选择 {} 个分区", self.state.selected_count()),
            None,
        )
        .width;
        let toolbar_height =
            selection_toolbar_height(width, &button_widths, summary_width, metrics);
        let list_height = preferred_list_height(self.state.volumes.len(), dpi, 3, 8);
        let status_height = if self.state.message.is_empty() {
            0
        } else {
            metrics.control_gap
                + measure_text(
                    self.shell.hwnd(),
                    self.font,
                    &self.state.message,
                    Some(width),
                )
                .height
                .max(metrics.label_height)
        };
        let content_height = toolbar_height + metrics.control_gap + list_height + status_height;
        self.shell
            .fit_content_height(pixels_to_logical(content_height, dpi));
        self.layout();
    }

    unsafe fn render_state(&mut self) {
        let _ = SendMessageW(
            self.controls.volumes,
            LVM_DELETEALLITEMS,
            WPARAM(0),
            LPARAM(0),
        );
        for (row, volume) in self.state.volumes.iter().enumerate() {
            for (column, value) in [
                volume.drive.clone(),
                if volume.label.trim().is_empty() {
                    "—".to_owned()
                } else {
                    volume.label.clone()
                },
                volume.file_system.clone(),
                format_size(volume.total_size_mb),
                format_size(volume.free_size_mb),
            ]
            .into_iter()
            .enumerate()
            {
                insert_list_item(self.controls.volumes, row as i32, column as i32, &value);
            }
        }
        self.render_selection();
        let controls_enabled = !self.state.loading && !self.state.volumes.is_empty();
        for control in [
            self.controls.select_all,
            self.controls.select_none,
            self.controls.invert,
            self.controls.volumes,
        ] {
            let _ = EnableWindow(control, controls_enabled);
        }
        let _ = ShowWindow(
            self.controls.load_status,
            if self.state.message.is_empty() {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        self.fit_and_layout();
    }

    unsafe fn render_selection(&self) {
        for (index, volume) in self.state.volumes.iter().enumerate() {
            set_item_checked(
                self.controls.volumes,
                index,
                self.state.is_selected(&volume.drive),
            );
        }
        self.render_selection_summary();
    }

    unsafe fn render_selection_summary(&self) {
        set_text(
            self.controls.selection_status,
            &crate::tr!("已选择 {} 个分区", self.state.selected_count()),
        );
        set_text(self.controls.load_status, &self.state.message);
        self.shell
            .set_primary_enabled(self.state.execution_intent().is_some());
    }

    unsafe fn sync_selection_from_list(&mut self) {
        for (index, volume) in self.state.volumes.iter().enumerate() {
            let checked = item_checked(self.controls.volumes, index);
            if checked {
                self.state.selected.insert(volume.drive.clone());
            } else {
                self.state.selected.remove(&volume.drive);
            }
        }
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        let _ = apply_list_view_theme(self.controls.volumes, palette);
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(
                self.controls.volumes,
                message,
                WPARAM(0),
                LPARAM(color.0 as isize),
            );
        }
        for button in [
            self.controls.select_all,
            self.controls.select_none,
            self.controls.invert,
        ] {
            apply_control_theme(button, palette, NativeControlKind::General);
        }
    }

    fn controls(&self) -> [HWND; 6] {
        let c = self.controls;
        [
            c.select_all,
            c.select_none,
            c.invert,
            c.selection_status,
            c.volumes,
            c.load_status,
        ]
    }
}

impl Drop for NativeBatchFormatDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<BatchFormatControls> {
    let button = |text: &str, id: u16| {
        child(
            parent,
            w!("BUTTON"),
            text,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            id,
        )
    };
    let volumes = child(
        parent,
        w!("SysListView32"),
        "",
        (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
        ID_VOLUME_LIST,
    )?;
    let _ = SendMessageW(
        volumes,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM(
            (LVS_EX_CHECKBOXES | LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP)
                as isize,
        ),
    );
    insert_columns(volumes);
    Ok(BatchFormatControls {
        select_all: button(&crate::tr!("全选"), ID_SELECT_ALL)?,
        select_none: button(&crate::tr!("全不选"), ID_SELECT_NONE)?,
        invert: button(&crate::tr!("反选"), ID_INVERT_SELECTION)?,
        selection_status: child(parent, w!("STATIC"), "", 0, ID_SELECTION_STATUS)?,
        volumes,
        load_status: child(parent, w!("STATIC"), "", 0, ID_LOAD_STATUS)?,
    })
}

unsafe fn insert_columns(list: HWND) {
    for (index, (title, width)) in [
        (crate::tr!("分区卷"), 90),
        (crate::tr!("卷标"), 150),
        (crate::tr!("文件系统"), 105),
        (crate::tr!("总空间"), 115),
        (crate::tr!("可用空间"), 115),
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

unsafe fn insert_list_item(list: HWND, row: i32, column: i32, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        iSubItem: column,
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

fn batch_format_column_widths(width: i32, dpi: u32) -> [i32; 5] {
    let s = |value: i32| scale(value, dpi);
    let width = width.max(0);
    let drive = s(82).min(width / 4);
    let file_system = s(104).min(width / 4);
    let total = s(112).min(width / 4);
    let free = s(112).min(width / 4);
    let label = (width - drive - file_system - total - free - s(4)).max(s(110));
    [drive, label, file_system, total, free]
}

fn format_size(value_mb: u64) -> String {
    format!("{:.1} GB", value_mb as f64 / 1024.0)
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

fn pixels_to_logical(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) - 1) / i64::from(dpi.max(1))) as i32
}

fn selection_toolbar_height(
    width: i32,
    button_widths: &[i32],
    summary_width: i32,
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
    if width - x >= summary_width + metrics.control_gap {
        buttons_height
    } else {
        buttons_height + metrics.tight_gap + metrics.label_height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volumes() -> Vec<BatchFormatVolume> {
        vec![
            BatchFormatVolume::new("D:", "Data", "NTFS", 200 * 1024, 80 * 1024),
            BatchFormatVolume::new("E:", "Backup", "ReFS", 500 * 1024, 300 * 1024),
        ]
    }

    #[test]
    fn selection_operations_preserve_inventory_order() {
        let mut state = BatchFormatDialogState::default();
        state.apply_inventory(Ok(volumes()));
        assert!(state.selected_drives().is_empty());

        state.select_all();
        assert_eq!(state.selected_drives(), ["D:", "E:"]);
        state.invert_selection();
        assert!(state.selected_drives().is_empty());
        state.set_selected("E:", true);
        assert_eq!(state.selected_drives(), ["E:"]);
        state.select_none();
        assert_eq!(state.selected_count(), 0);
    }

    #[test]
    fn intent_uses_legacy_fixed_file_system_and_label() {
        let mut state = BatchFormatDialogState::default();
        state.apply_inventory(Ok(volumes()));
        state.set_selected("E:", true);
        assert_eq!(
            state.execution_intent(),
            Some(MutatingToolIntent::BatchFormat {
                partitions: vec!["E:".into()],
                file_system: "NTFS".into(),
                volume_label: "新加卷".into(),
            })
        );
    }

    #[test]
    fn loading_empty_and_stale_selection_never_emit_intent() {
        let mut state = BatchFormatDialogState::default();
        assert_eq!(state.execution_intent(), None);
        state.apply_inventory(Ok(volumes()));
        state.set_selected("D:", true);
        state.apply_inventory(Ok(vec![BatchFormatVolume::new(
            "E:", "Backup", "NTFS", 10, 5,
        )]));
        assert_eq!(state.execution_intent(), None);
        state.begin_refresh();
        assert!(state.loading);
        assert!(state.volumes.is_empty());
    }

    #[test]
    fn inventory_filters_protected_invalid_and_duplicate_drives() {
        let mut input = volumes();
        input.push(BatchFormatVolume::new("c", "System", "NTFS", 10, 5));
        input.push(BatchFormatVolume::new("X:\\", "PE", "NTFS", 10, 5));
        input.push(BatchFormatVolume::new("d:\\", "Duplicate", "NTFS", 10, 5));
        input.push(BatchFormatVolume::new("not-a-drive", "Bad", "NTFS", 10, 5));
        assert_eq!(
            sanitize_inventory(input)
                .into_iter()
                .map(|volume| volume.drive)
                .collect::<Vec<_>>(),
            ["D:", "E:"]
        );
    }

    #[test]
    fn responsive_columns_keep_all_fields_visible() {
        let normal = batch_format_column_widths(620, 96);
        assert_eq!(normal.iter().sum::<i32>(), 616);
        assert!(normal.into_iter().all(|width| width > 0));

        let high_dpi = batch_format_column_widths(1240, 192);
        assert_eq!(high_dpi.iter().sum::<i32>(), 1232);
        assert!(high_dpi[1] >= normal[1] * 2 - 8);
    }
}
