//! Dedicated native UI for the legacy partition-to-partition copy workflow.
//!
//! The host supplies an already filtered, read-only volume inventory and resume/progress updates.
//! This module never enumerates a disk or copies a file. It only produces refresh, confirmation
//! and close intents containing the existing strongly typed [`PartitionCopyRequest`].

use std::collections::{BTreeSet, VecDeque};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY,
    WM_SETFONT, WS_BORDER, WS_TABSTOP, WS_VSCROLL,
};

use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{measure_text, preferred_list_height, LayoutMetrics};
use super::super::theme::{
    apply_control_theme, apply_list_view_theme, apply_progress_theme, NativeControlKind, Palette,
};
use crate::core::native_partition_copy::{
    PartitionCopyInventoryItem, PartitionCopyProgress, PartitionCopyRequest,
};

const ID_SOURCE_COMBO: u16 = 65_100;
const ID_SOURCE_LIST: u16 = 65_101;
const ID_TARGET_COMBO: u16 = 65_102;
const ID_TARGET_LIST: u16 = 65_103;
const ID_RESUME_STATUS: u16 = 65_104;
const ID_PROGRESS_STATUS: u16 = 65_105;
const ID_PROGRESS: u16 = 65_106;
const ID_LOG: u16 = 65_107;
const LOG_CONTROL_KIND: NativeControlKind = NativeControlKind::ScrollableField;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PartitionCopyFlexibleHeights {
    list: i32,
    log: i32,
}

fn fit_partition_copy_flexible_heights(
    available_height: i32,
    fixed_height: i32,
    preferred_list: i32,
    preferred_log: i32,
    minimum_list: i32,
    minimum_log: i32,
) -> PartitionCopyFlexibleHeights {
    let budget = available_height.saturating_sub(fixed_height).max(0);
    let preferred_list = preferred_list.max(0);
    let preferred_log = preferred_log.max(0);
    let minimum_list = minimum_list.clamp(0, preferred_list);
    let minimum_log = minimum_log.clamp(0, preferred_log);
    let preferred_total = preferred_list
        .saturating_mul(2)
        .saturating_add(preferred_log);
    if budget >= preferred_total {
        return PartitionCopyFlexibleHeights {
            list: preferred_list,
            log: preferred_log,
        };
    }

    let minimum_total = minimum_list.saturating_mul(2).saturating_add(minimum_log);
    if budget <= minimum_total {
        if minimum_total == 0 {
            return PartitionCopyFlexibleHeights { list: 0, log: 0 };
        }
        let list =
            ((i64::from(budget) * i64::from(minimum_list)) / i64::from(minimum_total)) as i32;
        return PartitionCopyFlexibleHeights {
            list,
            log: budget.saturating_sub(list.saturating_mul(2)),
        };
    }

    let mut list = minimum_list;
    let mut log = minimum_log;
    let mut remaining = budget - minimum_total;
    let list_capacity = preferred_list - minimum_list;
    let log_capacity = preferred_log - minimum_log;
    let total_capacity = list_capacity.saturating_mul(2).saturating_add(log_capacity);
    if total_capacity > 0 {
        let pair_extra = ((i64::from(remaining) * i64::from(list_capacity.saturating_mul(2)))
            / i64::from(total_capacity)) as i32;
        let per_list = (pair_extra / 2).min(list_capacity);
        list += per_list;
        remaining -= per_list * 2;
        let log_extra = remaining.min(log_capacity);
        log += log_extra;
        remaining -= log_extra;
    }
    let additional_list = (remaining / 2).min(preferred_list - list);
    list += additional_list;
    remaining -= additional_list * 2;
    log += remaining.min(preferred_log - log);
    PartitionCopyFlexibleHeights { list, log }
}

const LVNI_SELECTED: isize = 0x0002;
const PBM_SETRANGE32: u32 = 0x0406;
const PBM_SETPOS: u32 = 0x0402;
const MAX_LOG_BYTES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionCopyInventoryRow {
    pub drive: String,
    pub total_size_mb: u64,
    pub used_size_mb: u64,
    pub free_size_mb: u64,
    pub label: String,
    pub has_system: bool,
}

impl PartitionCopyInventoryRow {
    pub fn new(
        drive: impl Into<String>,
        total_size_mb: u64,
        used_size_mb: u64,
        free_size_mb: u64,
        label: impl Into<String>,
        has_system: bool,
    ) -> Self {
        Self {
            drive: drive.into(),
            total_size_mb,
            used_size_mb,
            free_size_mb,
            label: label.into(),
            has_system,
        }
    }

    fn columns(&self) -> [String; 5] {
        [
            self.drive.clone(),
            format_size(self.total_size_mb),
            format_size(self.used_size_mb),
            display_label(&self.label),
            if self.has_system {
                crate::tr!("有系统")
            } else {
                crate::tr!("无系统")
            },
        ]
    }
}

impl From<PartitionCopyInventoryItem> for PartitionCopyInventoryRow {
    fn from(item: PartitionCopyInventoryItem) -> Self {
        Self {
            drive: item.drive,
            total_size_mb: item.total_size_mb,
            used_size_mb: item.used_size_mb,
            free_size_mb: item.free_size_mb,
            label: item.label,
            has_system: item.has_system,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PartitionCopyResumeState {
    #[default]
    Unchecked,
    NewCopy,
    Resumable,
    Unavailable(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BoundedCopyLog {
    lines: VecDeque<String>,
    byte_len: usize,
}

impl BoundedCopyLog {
    fn push(&mut self, line: String) {
        self.byte_len = self.byte_len.saturating_add(line.len() + 2);
        self.lines.push_back(line);
        while self.byte_len > MAX_LOG_BYTES {
            let Some(removed) = self.lines.pop_front() else {
                break;
            };
            self.byte_len = self.byte_len.saturating_sub(removed.len() + 2);
        }
    }

    fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\r\n")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionCopyDialogState {
    pub loading: bool,
    pub copying: bool,
    pub inventory: Vec<PartitionCopyInventoryRow>,
    pub source: Option<String>,
    pub target: Option<String>,
    pub resume: PartitionCopyResumeState,
    pub progress: PartitionCopyProgress,
    pub message: String,
    log: BoundedCopyLog,
}

impl Default for PartitionCopyDialogState {
    fn default() -> Self {
        Self {
            loading: true,
            copying: false,
            inventory: Vec::new(),
            source: None,
            target: None,
            resume: PartitionCopyResumeState::Unchecked,
            progress: PartitionCopyProgress::default(),
            message: crate::tr!("正在检测分区..."),
            log: BoundedCopyLog::default(),
        }
    }
}

impl PartitionCopyDialogState {
    pub fn begin_refresh(&mut self) {
        self.loading = true;
        self.inventory.clear();
        self.source = None;
        self.target = None;
        self.resume = PartitionCopyResumeState::Unchecked;
        self.message = crate::tr!("正在检测分区...");
    }

    pub fn apply_inventory(&mut self, result: Result<Vec<PartitionCopyInventoryRow>, String>) {
        self.loading = false;
        match result {
            Ok(rows) => {
                self.inventory = sanitize_inventory(rows);
                retain_available(&self.inventory, &mut self.source);
                retain_available(&self.inventory, &mut self.target);
                self.resume = PartitionCopyResumeState::Unchecked;
                self.message = if self.inventory.is_empty() {
                    crate::tr!("未找到可用的分区")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.inventory.clear();
                self.source = None;
                self.target = None;
                self.resume = PartitionCopyResumeState::Unchecked;
                self.message = crate::tr!("加载失败：{}", error);
            }
        }
    }

    pub fn set_source(&mut self, value: Option<&str>) {
        let source = inventory_drive(&self.inventory, value);
        self.source = source.filter(|source| {
            self.target
                .as_deref()
                .is_none_or(|target| !source.eq_ignore_ascii_case(target))
        });
        self.resume = PartitionCopyResumeState::Unchecked;
    }

    pub fn set_target(&mut self, value: Option<&str>) {
        let target = inventory_drive(&self.inventory, value);
        self.target = target.filter(|target| {
            self.source
                .as_deref()
                .is_none_or(|source| !target.eq_ignore_ascii_case(source))
        });
        self.resume = PartitionCopyResumeState::Unchecked;
    }

    pub fn request(&self) -> Option<PartitionCopyRequest> {
        if self.loading || self.copying {
            return None;
        }
        let source = self.source.as_ref()?;
        let target = self.target.as_ref()?;
        (!source.eq_ignore_ascii_case(target)).then(|| PartitionCopyRequest {
            source: source.clone(),
            target: target.clone(),
        })
    }

    pub fn progress_percent(&self) -> usize {
        if self.progress.total_count == 0 {
            return usize::from(self.progress.completed) * 100;
        }
        let processed = self
            .progress
            .copied_count
            .saturating_add(self.progress.skipped_count)
            .saturating_add(self.progress.failed_count);
        (processed.saturating_mul(100) / self.progress.total_count.max(1)).min(100)
    }

    pub fn apply_progress(&mut self, progress: PartitionCopyProgress) {
        if !progress.current_file.is_empty() {
            let action = if progress.completed {
                crate::tr!("完成")
            } else {
                crate::tr!("复制")
            };
            self.log
                .push(format!("[{action}] {}", progress.current_file));
        }
        self.copying = !progress.completed && progress.error.is_none();
        self.message = progress_message(&progress);
        self.progress = progress;
    }

    pub fn log_text(&self) -> String {
        self.log.text()
    }
}

fn sanitize_inventory(rows: Vec<PartitionCopyInventoryRow>) -> Vec<PartitionCopyInventoryRow> {
    let mut seen = BTreeSet::new();
    rows.into_iter()
        .filter_map(|mut row| {
            row.drive = normalize_drive(&row.drive)?;
            seen.insert(row.drive.clone()).then_some(row)
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

fn inventory_drive(inventory: &[PartitionCopyInventoryRow], value: Option<&str>) -> Option<String> {
    let value = normalize_drive(value?)?;
    inventory
        .iter()
        .find(|row| row.drive.eq_ignore_ascii_case(&value))
        .map(|row| row.drive.clone())
}

fn retain_available(inventory: &[PartitionCopyInventoryRow], selected: &mut Option<String>) {
    *selected = inventory_drive(inventory, selected.as_deref());
}

fn progress_message(progress: &PartitionCopyProgress) -> String {
    if let Some(error) = &progress.error {
        return crate::tr!("错误: {}", error);
    }
    if progress.completed {
        if progress.failed_count > 0 {
            return crate::tr!(
                "复制完成！已复制 {} 个文件，跳过 {} 个，失败 {} 个",
                progress.copied_count,
                progress.skipped_count,
                progress.failed_count
            );
        }
        return crate::tr!(
            "复制完成！已复制 {} 个文件，跳过 {} 个（已存在）",
            progress.copied_count,
            progress.skipped_count
        );
    }
    crate::tr!(
        "正在复制 {}/{}（跳过 {}）: {}",
        progress.copied_count,
        progress.total_count,
        progress.skipped_count,
        progress.current_file
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartitionCopyDialogIntent {
    RefreshInventory,
    RequestConfirmation(PartitionCopyRequest),
    Close,
}

#[derive(Clone, Copy)]
struct Controls {
    source_label: HWND,
    source_combo: HWND,
    source_list: HWND,
    target_label: HWND,
    target_combo: HWND,
    target_list: HWND,
    resume_status: HWND,
    progress_status: HWND,
    progress: HWND,
    log_label: HWND,
    log: HWND,
}

pub struct NativePartitionCopyDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: PartitionCopyDialogState,
    font: HFONT,
}

impl NativePartitionCopyDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let mut shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("分区对拷"),
                title: crate::tr!("分区对拷"),
                description: crate::tr!("将源分区的所有文件复制到目标分区（支持断点续传）"),
                width: 760,
                height: 690,
                buttons: DialogButtons {
                    primary: crate::tr!("开始对拷"),
                    secondary: Some(crate::tr!("刷新")),
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
        shell.set_primary_closes(false);
        shell.set_secondary_closes(false);
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
            state: PartitionCopyDialogState::default(),
            font,
        };
        dialog.apply_font_and_theme();
        dialog.layout();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &PartitionCopyDialogState {
        &self.state
    }

    pub fn owns_choice(&self, control: HWND) -> bool {
        control == self.controls.source_combo || control == self.controls.target_combo
    }

    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.controls.source_list || control == self.controls.target_list
    }

    pub unsafe fn handle_choice_changed(&mut self, control: HWND) -> Option<PartitionCopyRequest> {
        if control == self.controls.source_combo {
            let selected =
                combo_selection(control, &self.state.inventory, self.state.target.as_deref());
            self.state.set_source(selected.as_deref());
        } else if control == self.controls.target_combo {
            let selected =
                combo_selection(control, &self.state.inventory, self.state.source.as_deref());
            self.state.set_target(selected.as_deref());
        } else {
            return None;
        }
        self.render_state();
        self.state.request()
    }

    pub unsafe fn handle_list_changed(&mut self, control: HWND) -> Option<PartitionCopyRequest> {
        if control == self.controls.source_list {
            let selected =
                selected_list_drive(control, &self.state.inventory, self.state.target.as_deref());
            self.state.set_source(selected.as_deref());
        } else if control == self.controls.target_list {
            let selected =
                selected_list_drive(control, &self.state.inventory, self.state.source.as_deref());
            self.state.set_target(selected.as_deref());
        } else {
            return None;
        }
        self.render_state();
        self.state.request()
    }

    pub unsafe fn set_inventory(&mut self, result: Result<Vec<PartitionCopyInventoryRow>, String>) {
        self.state.apply_inventory(result);
        self.render_state();
    }

    pub unsafe fn set_resume_state(&mut self, state: PartitionCopyResumeState) {
        self.state.resume = state;
        self.render_status();
    }

    pub unsafe fn set_copying(&mut self, copying: bool) {
        self.state.copying = copying;
        self.render_status();
    }

    pub unsafe fn apply_progress(&mut self, progress: PartitionCopyProgress) {
        self.state.apply_progress(progress);
        self.render_progress();
        self.render_status();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
        // The dialog shell performs a final generic descendant pass immediately before the
        // first frame.  Reassert the dedicated multiline/report roles afterwards so the copy
        // log keeps DarkMode_Explorer (including its scrollbar) instead of being treated as a
        // single-line CFD field.
        self.apply_font_and_theme();
    }

    pub unsafe fn take_intent(&mut self) -> Option<PartitionCopyDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Cancel => Some(PartitionCopyDialogIntent::Close),
            DialogResult::Secondary if !self.state.copying => {
                self.state.begin_refresh();
                self.render_state();
                Some(PartitionCopyDialogIntent::RefreshInventory)
            }
            DialogResult::Secondary => None,
            DialogResult::Primary => match self.state.request() {
                Some(request) => {
                    self.shell.hide_modeless();
                    Some(PartitionCopyDialogIntent::RequestConfirmation(request))
                }
                None => {
                    self.state.message =
                        if self.state.source == self.state.target && self.state.source.is_some() {
                            crate::tr!("源分区和目标分区不能相同！")
                        } else {
                            crate::tr!("请选择源分区和目标分区。")
                        };
                    self.render_status();
                    None
                }
            },
        }
    }

    pub unsafe fn layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_width = [crate::tr!("请选择源分区:"), crate::tr!("请选择目标分区:")]
            .iter()
            .map(|label| measure_text(self.shell.hwnd(), self.font, label, None).width)
            .max()
            .unwrap_or(0)
            .min(width / 3);
        let value_x = label_width + metrics.control_gap;
        let value_width = (width - value_x).max(0);
        let preferred_list = preferred_list_height(self.state.inventory.len(), dpi, 3, 6);

        let row_height = metrics.field_height;
        let list_y = row_height + metrics.control_gap;
        let progress_height = if self.state.message.is_empty() {
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
        let progress_bar_height = scale(18, dpi);
        let fixed_height = list_y * 2
            + metrics.section_gap
            + metrics.control_gap
            + metrics.label_height
            + metrics.tight_gap
            + progress_height
            + if progress_height > 0 {
                metrics.tight_gap
            } else {
                0
            }
            + progress_bar_height
            + metrics.control_gap
            + metrics.label_height
            + metrics.tight_gap;
        let preferred_log =
            metrics.list_row_height * self.state.log.lines.len().clamp(3, 6) as i32 + scale(2, dpi);
        let natural_height = fixed_height + preferred_list * 2 + preferred_log;
        self.shell
            .fit_content_height(logical_height(natural_height, dpi));

        // The top-level shell can be clamped to the monitor work area. Read the resulting content
        // height and keep every tool control above the shell-owned command bar.
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let available_height = (rect.bottom - rect.top).max(0);
        let flexible = fit_partition_copy_flexible_heights(
            available_height,
            fixed_height,
            preferred_list,
            preferred_log,
            metrics.list_row_height + scale(2, dpi),
            metrics.list_row_height + scale(2, dpi),
        );
        let list_height = flexible.list;
        let log_height = flexible.log;
        move_control(
            self.controls.source_label,
            0,
            ((row_height - metrics.label_height) / 2).max(0),
            label_width,
            metrics.label_height,
        );
        move_control(
            self.controls.source_combo,
            value_x,
            0,
            value_width,
            scale(220, dpi),
        );
        move_control(self.controls.source_list, 0, list_y, width, list_height);
        let target_y = list_y + list_height + metrics.section_gap;
        move_control(
            self.controls.target_label,
            0,
            target_y + ((row_height - metrics.label_height) / 2).max(0),
            label_width,
            metrics.label_height,
        );
        move_control(
            self.controls.target_combo,
            value_x,
            target_y,
            value_width,
            scale(220, dpi),
        );
        move_control(
            self.controls.target_list,
            0,
            target_y + list_y,
            width,
            list_height,
        );
        let status_y = target_y + list_y + list_height + metrics.control_gap;
        let resume_height = metrics.label_height;
        move_control(
            self.controls.resume_status,
            0,
            status_y,
            width,
            resume_height,
        );
        let mut y = status_y + resume_height + metrics.tight_gap;
        move_control(self.controls.progress_status, 0, y, width, progress_height);
        y += progress_height;
        if progress_height > 0 {
            y += metrics.tight_gap;
        }
        move_control(self.controls.progress, 0, y, width, progress_bar_height);
        y += progress_bar_height + metrics.control_gap;
        move_control(self.controls.log_label, 0, y, width, metrics.label_height);
        y += metrics.label_height + metrics.tight_gap;
        move_control(self.controls.log, 0, y, width, log_height);
        y += log_height;
        debug_assert!(y <= available_height || available_height < fixed_height);
        for list in [self.controls.source_list, self.controls.target_list] {
            let mut client = RECT::default();
            let _ = GetClientRect(list, &mut client);
            let list_width = (client.right - client.left).max(0);
            for (index, column_width) in partition_columns(list_width, dpi).into_iter().enumerate()
            {
                let _ = SendMessageW(
                    list,
                    LVM_SETCOLUMNWIDTH,
                    WPARAM(index),
                    LPARAM(column_width as isize),
                );
            }
        }
    }

    unsafe fn render_state(&mut self) {
        refill_combo(
            self.controls.source_combo,
            &self.state.inventory,
            self.state.source.as_deref(),
            self.state.target.as_deref(),
        );
        refill_combo(
            self.controls.target_combo,
            &self.state.inventory,
            self.state.target.as_deref(),
            self.state.source.as_deref(),
        );
        refill_list(
            self.controls.source_list,
            &self.state.inventory,
            self.state.target.as_deref(),
        );
        refill_list(
            self.controls.target_list,
            &self.state.inventory,
            self.state.source.as_deref(),
        );
        self.render_progress();
        self.render_status();
    }

    unsafe fn render_status(&self) {
        let resume = match &self.state.resume {
            PartitionCopyResumeState::Unchecked => {
                crate::tr!("断点状态：等待选择源分区和目标分区")
            }
            PartitionCopyResumeState::NewCopy => crate::tr!("断点状态：将开始新的对拷"),
            PartitionCopyResumeState::Resumable => crate::tr!("断点状态：检测到可续传任务"),
            PartitionCopyResumeState::Unavailable(reason) => crate::tr!("断点状态：{}", reason),
        };
        set_text(self.controls.resume_status, &resume);
        set_text(self.controls.progress_status, &self.state.message);
        let choices_enabled = !self.state.loading && !self.state.copying;
        for control in [
            self.controls.source_combo,
            self.controls.source_list,
            self.controls.target_combo,
            self.controls.target_list,
        ] {
            let _ = EnableWindow(control, choices_enabled);
        }
        self.shell
            .set_primary_enabled(self.state.request().is_some());
    }

    unsafe fn render_progress(&self) {
        let percent = self.state.progress_percent().min(100);
        let _ = SendMessageW(
            self.controls.progress,
            PBM_SETRANGE32,
            WPARAM(0),
            LPARAM(100),
        );
        let _ = SendMessageW(
            self.controls.progress,
            PBM_SETPOS,
            WPARAM(percent),
            LPARAM(0),
        );
        set_text(self.controls.log, &self.state.log_text());
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        for list in [self.controls.source_list, self.controls.target_list] {
            let _ = apply_list_view_theme(list, palette);
            for (message, color) in [
                (LVM_SETBKCOLOR, palette.edit),
                (LVM_SETTEXTBKCOLOR, palette.edit),
                (LVM_SETTEXTCOLOR, palette.text),
            ] {
                let _ = SendMessageW(list, message, WPARAM(0), LPARAM(color.0 as isize));
            }
        }
        for combo in [self.controls.source_combo, self.controls.target_combo] {
            apply_control_theme(combo, palette, NativeControlKind::Field);
        }
        apply_control_theme(self.controls.log, palette, LOG_CONTROL_KIND);
        apply_progress_theme(self.controls.progress, palette);
    }

    fn controls(&self) -> [HWND; 11] {
        let c = self.controls;
        [
            c.source_label,
            c.source_combo,
            c.source_list,
            c.target_label,
            c.target_combo,
            c.target_list,
            c.resume_status,
            c.progress_status,
            c.progress,
            c.log_label,
            c.log,
        ]
    }
}

impl Drop for NativePartitionCopyDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<Controls> {
    let source_list = create_list(parent, ID_SOURCE_LIST)?;
    let target_list = create_list(parent, ID_TARGET_LIST)?;
    Ok(Controls {
        source_label: child(parent, w!("STATIC"), &crate::tr!("请选择源分区:"), 0, 0)?,
        source_combo: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_SOURCE_COMBO,
        )?,
        source_list,
        target_label: child(parent, w!("STATIC"), &crate::tr!("请选择目标分区:"), 0, 0)?,
        target_combo: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_TARGET_COMBO,
        )?,
        target_list,
        resume_status: child(parent, w!("STATIC"), "", 0, ID_RESUME_STATUS)?,
        progress_status: child(parent, w!("STATIC"), "", 0, ID_PROGRESS_STATUS)?,
        progress: child(parent, w!("msctls_progress32"), "", 0, ID_PROGRESS)?,
        log_label: child(parent, w!("STATIC"), &crate::tr!("复制日志:"), 0, 0)?,
        log: child(
            parent,
            w!("EDIT"),
            "",
            ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY | WS_VSCROLL.0 as i32 | WS_BORDER.0 as i32,
            ID_LOG,
        )?,
    })
}

unsafe fn create_list(parent: HWND, id: u16) -> windows::core::Result<HWND> {
    let list = child(
        parent,
        w!("SysListView32"),
        "",
        (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
        id,
    )?;
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
    );
    for (index, title) in ["分区卷", "总空间", "已用空间", "卷标", "状态"]
        .into_iter()
        .enumerate()
    {
        let mut text = wide(crate::tr!(title));
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: 110,
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
    Ok(list)
}

unsafe fn refill_combo(
    combo: HWND,
    inventory: &[PartitionCopyInventoryRow],
    selected: Option<&str>,
    excluded: Option<&str>,
) {
    let _ = SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for row in inventory
        .iter()
        .filter(|row| !drive_matches(&row.drive, excluded))
    {
        add_combo_item(combo, &row.drive);
    }
    select_combo(combo, selected, inventory, excluded);
}

unsafe fn select_combo(
    combo: HWND,
    selected: Option<&str>,
    inventory: &[PartitionCopyInventoryRow],
    excluded: Option<&str>,
) {
    let index = selected
        .and_then(|selected| {
            inventory
                .iter()
                .filter(|row| !drive_matches(&row.drive, excluded))
                .position(|row| row.drive.eq_ignore_ascii_case(selected))
        })
        .map_or(NO_COMBO_SELECTION, |index| index);
    let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
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

unsafe fn combo_selection(
    combo: HWND,
    inventory: &[PartitionCopyInventoryRow],
    excluded: Option<&str>,
) -> Option<String> {
    let index = SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    let choice_count = inventory
        .iter()
        .filter(|row| !drive_matches(&row.drive, excluded))
        .count();
    combo_inventory_index(index, choice_count)
        .and_then(|index| inventory_choice(inventory, excluded, index))
        .map(|row| row.drive.clone())
}

fn inventory_choice<'a>(
    inventory: &'a [PartitionCopyInventoryRow],
    excluded: Option<&str>,
    index: usize,
) -> Option<&'a PartitionCopyInventoryRow> {
    inventory
        .iter()
        .filter(|row| !drive_matches(&row.drive, excluded))
        .nth(index)
}

fn drive_matches(drive: &str, candidate: Option<&str>) -> bool {
    candidate.is_some_and(|candidate| drive.eq_ignore_ascii_case(candidate))
}

unsafe fn refill_list(list: HWND, inventory: &[PartitionCopyInventoryRow], excluded: Option<&str>) {
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    for (row, item) in inventory
        .iter()
        .filter(|item| !drive_matches(&item.drive, excluded))
        .enumerate()
    {
        for (column, value) in item.columns().into_iter().enumerate() {
            let mut value = wide(value);
            let mut list_item = LVITEMW {
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
                LPARAM((&mut list_item as *mut LVITEMW) as isize),
            );
        }
    }
}

unsafe fn selected_list_drive(
    list: HWND,
    inventory: &[PartitionCopyInventoryRow],
    excluded: Option<&str>,
) -> Option<String> {
    let index = SendMessageW(
        list,
        LVM_GETNEXTITEM,
        WPARAM(usize::MAX),
        LPARAM(LVNI_SELECTED),
    )
    .0;
    (index >= 0)
        .then(|| inventory_choice(inventory, excluded, index as usize).map(|row| row.drive.clone()))
        .flatten()
}

fn partition_columns(width: i32, dpi: u32) -> [i32; 5] {
    let usable = (width - scale(4, dpi)).max(0);
    let drive = usable * 14 / 100;
    let total = usable * 19 / 100;
    let used = usable * 19 / 100;
    let status = usable * 16 / 100;
    let label = usable - drive - total - used - status;
    [drive, total, used, label, status]
}

fn format_size(value_mb: u64) -> String {
    format!("{:.1} GB", value_mb as f64 / 1024.0)
}

fn display_label(label: &str) -> String {
    if label.trim().is_empty() {
        "—".to_owned()
    } else {
        label.to_owned()
    }
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
    fn copy_log_keeps_scrollable_field_theme_role() {
        assert_eq!(LOG_CONTROL_KIND, NativeControlKind::ScrollableField);
    }

    #[test]
    fn clamped_dialog_distributes_flexible_height_without_crossing_the_command_bar() {
        assert_eq!(
            fit_partition_copy_flexible_heights(700, 220, 140, 120, 70, 50),
            PartitionCopyFlexibleHeights {
                list: 140,
                log: 120,
            }
        );
        let compact = fit_partition_copy_flexible_heights(500, 220, 140, 120, 70, 50);
        assert!(compact.list >= 70 && compact.list <= 140);
        assert!(compact.log >= 50 && compact.log <= 120);
        assert!(compact.list * 2 + compact.log <= 280);

        let severely_clamped = fit_partition_copy_flexible_heights(250, 220, 140, 120, 70, 50);
        assert!(severely_clamped.list >= 0);
        assert!(severely_clamped.log >= 0);
        assert_eq!(severely_clamped.list * 2 + severely_clamped.log, 30);
    }

    fn rows() -> Vec<PartitionCopyInventoryRow> {
        vec![
            PartitionCopyInventoryRow::new(
                "d:\\",
                200 * 1024,
                120 * 1024,
                80 * 1024,
                "Data",
                false,
            ),
            PartitionCopyInventoryRow::new(
                "E:",
                300 * 1024,
                60 * 1024,
                240 * 1024,
                "Windows",
                true,
            ),
        ]
    }

    #[test]
    fn source_and_target_default_to_unselected_and_must_differ() {
        let mut state = PartitionCopyDialogState::default();
        assert!(
            state.loading,
            "opening the dialog must begin with async preload state"
        );
        state.apply_inventory(Ok(rows()));
        assert_eq!(state.source, None);
        assert_eq!(state.target, None);
        assert_eq!(state.request(), None);
        state.set_source(Some("D:"));
        state.set_target(Some("D:"));
        assert_eq!(state.source.as_deref(), Some("D:"));
        assert_eq!(state.target, None);
        assert_eq!(state.request(), None);
        state.set_target(Some("E:"));
        assert_eq!(
            state.request(),
            Some(PartitionCopyRequest {
                source: "D:".into(),
                target: "E:".into()
            })
        );
    }

    #[test]
    fn opposite_partition_is_removed_without_shifting_inventory_mapping() {
        let mut inventory = sanitize_inventory(rows());
        inventory.push(PartitionCopyInventoryRow::new(
            "F:",
            400 * 1024,
            100 * 1024,
            300 * 1024,
            "Archive",
            false,
        ));

        assert_eq!(
            inventory_choice(&inventory, Some("E:"), 0).map(|row| row.drive.as_str()),
            Some("D:")
        );
        assert_eq!(
            inventory_choice(&inventory, Some("E:"), 1).map(|row| row.drive.as_str()),
            Some("F:")
        );
        assert!(inventory_choice(&inventory, Some("E:"), 2).is_none());
    }

    #[test]
    fn changing_one_side_never_allows_the_same_partition_on_the_other_side() {
        let mut state = PartitionCopyDialogState::default();
        state.apply_inventory(Ok(rows()));
        state.set_target(Some("E:"));
        state.set_source(Some("E:"));
        assert_eq!(state.source, None);
        assert_eq!(state.target.as_deref(), Some("E:"));

        state.set_source(Some("D:"));
        state.set_target(Some("D:"));
        assert_eq!(state.source.as_deref(), Some("D:"));
        assert_eq!(state.target, None);
    }

    #[test]
    fn refresh_drops_stale_selections_and_rejects_non_inventory_values() {
        let mut state = PartitionCopyDialogState::default();
        state.apply_inventory(Ok(rows()));
        state.set_source(Some("D:"));
        state.set_target(Some("E:"));
        state.apply_inventory(Ok(vec![rows().remove(1)]));
        assert_eq!(state.source, None);
        assert_eq!(state.target.as_deref(), Some("E:"));
        state.set_source(Some("Z:"));
        assert_eq!(state.source, None);
    }

    #[test]
    fn inventory_columns_restore_legacy_details() {
        let columns = rows()[1].columns();
        assert_eq!(columns[0], "E:");
        assert_eq!(columns[1], "300.0 GB");
        assert_eq!(columns[2], "60.0 GB");
        assert_eq!(columns[3], "Windows");
        assert_eq!(columns[4], crate::tr!("有系统"));
    }

    #[test]
    fn progress_and_utf8_log_are_bounded() {
        let mut state = PartitionCopyDialogState::default();
        for index in 0..4_000 {
            state.apply_progress(PartitionCopyProgress {
                current_file: format!("目录/中文文件-{index:04}.bin"),
                copied_count: index,
                total_count: 4_000,
                ..Default::default()
            });
        }
        assert!(state.log.byte_len <= MAX_LOG_BYTES);
        assert!(state.log_text().is_char_boundary(state.log_text().len()));
        assert!(state.progress_percent() <= 100);
    }

    #[test]
    fn terminal_progress_distinguishes_failures_and_completes_bar() {
        let mut state = PartitionCopyDialogState::default();
        state.apply_progress(PartitionCopyProgress {
            copied_count: 8,
            total_count: 10,
            skipped_count: 1,
            failed_count: 1,
            completed: true,
            ..Default::default()
        });
        assert_eq!(state.progress_percent(), 100);
        assert!(!state.copying);
        assert!(state.message.contains(&crate::tr!("失败")));
    }

    #[test]
    fn columns_scale_without_losing_the_flexible_label_column() {
        for dpi in [96, 144, 192] {
            for logical_width in [320, 680] {
                let width = scale(logical_width, dpi);
                let columns = partition_columns(width, dpi);
                assert!(columns.into_iter().all(|column| column > 0));
                assert!(columns.into_iter().sum::<i32>() <= width);
            }
        }
    }
}
