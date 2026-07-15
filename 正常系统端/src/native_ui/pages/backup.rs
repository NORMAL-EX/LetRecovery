//! Native system-backup page.
//!
//! This module only presents and collects backup intent. Starting a backup remains the
//! controller's responsibility so a window notification can never directly perform disk I/O.

use std::cell::RefCell;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW, ShowWindow,
    BM_GETCHECK, BS_AUTOCHECKBOX, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOHSCROLL, ES_NUMBER, HMENU, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_CHILD, WS_TABSTOP,
};

use super::super::controls::{child, wide};
use super::super::layout::{centered_control_y_ceil, LayoutMetrics};
use super::super::theme::{
    apply_control_theme, apply_list_view_theme, combo_closed_height, NativeControlKind, Palette,
};
use crate::core::install_config::BackupConfig;

pub const ID_SOURCE_LIST: u16 = 410;
pub const ID_FORMAT: u16 = 411;
pub const ID_SWM_SIZE: u16 = 412;
pub const ID_SAVE_PATH: u16 = 413;
pub const ID_BROWSE: u16 = 414;
pub const ID_NAME: u16 = 415;
pub const ID_DESCRIPTION: u16 = 416;
pub const ID_INCREMENTAL: u16 = 417;
pub const ID_PE: u16 = 425;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupFormat {
    #[default]
    Wim,
    Esd,
    Swm,
    Gho,
}

impl BackupFormat {
    pub const fn to_config_value(self) -> u8 {
        match self {
            Self::Wim => 0,
            Self::Esd => 1,
            Self::Swm => 2,
            Self::Gho => 3,
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Wim => "wim",
            Self::Esd => "esd",
            Self::Swm => "swm",
            Self::Gho => "gho",
        }
    }

    pub fn filter_description(self) -> String {
        match self {
            Self::Wim => crate::tr!("WIM 镜像"),
            Self::Esd => crate::tr!("ESD 镜像"),
            Self::Swm => crate::tr!("SWM 分卷镜像"),
            Self::Gho => crate::tr!("Ghost 镜像"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPageState {
    pub source_partition: Option<usize>,
    pub format: BackupFormat,
    pub swm_split_size_mb: u32,
    pub save_path: String,
    pub name: String,
    pub description: String,
    pub incremental: bool,
}

impl Default for BackupPageState {
    fn default() -> Self {
        Self {
            source_partition: None,
            format: BackupFormat::Wim,
            swm_split_size_mb: 4096,
            save_path: String::new(),
            name: String::new(),
            description: String::new(),
            incremental: false,
        }
    }
}

/// Builds the two generated backup fields in the language that is active at the time of use.
///
/// The timestamp is supplied by the caller so switching the interface language does not make an
/// otherwise untouched backup look like a newly-created task.
pub fn localized_backup_defaults(timestamp: &str) -> (String, String) {
    (
        crate::tr!("系统备份_{}", timestamp),
        crate::tr!("使用 LetRecovery 创建的系统备份"),
    )
}

/// Replaces generated text after a language switch, while preserving anything the user typed.
fn relocalize_generated_value(
    current: String,
    previous_generated: &str,
    next_generated: &str,
) -> String {
    if current == previous_generated {
        next_generated.to_owned()
    } else {
        current
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupValidationError {
    SourcePartitionRequired,
    SourcePartitionUnavailable { index: usize },
    SourcePartitionInvalid,
    SavePathRequired,
    NameRequired,
    SwmSplitSizeOutOfRange { value: u32 },
}

impl std::fmt::Display for BackupValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourcePartitionRequired => formatter.write_str(&crate::tr!("请选择要备份的分区")),
            Self::SourcePartitionUnavailable { .. } => {
                formatter.write_str(&crate::tr!("所选备份分区已不可用，请重新选择"))
            }
            Self::SourcePartitionInvalid => {
                formatter.write_str(&crate::tr!("所选备份分区没有有效盘符"))
            }
            Self::SavePathRequired => formatter.write_str(&crate::tr!("请选择备份保存位置")),
            Self::NameRequired => formatter.write_str(&crate::tr!("请输入备份名称")),
            Self::SwmSplitSizeOutOfRange { .. } => {
                formatter.write_str(&crate::tr!("SWM 分卷大小必须在 512 到 8192 MB 之间"))
            }
        }
    }
}

impl std::error::Error for BackupValidationError {}

impl BackupPageState {
    pub fn validate(&self, partitions: &[BackupPartitionRow]) -> Result<(), BackupValidationError> {
        let index = self
            .source_partition
            .ok_or(BackupValidationError::SourcePartitionRequired)?;
        let partition = partitions
            .get(index)
            .ok_or(BackupValidationError::SourcePartitionUnavailable { index })?;
        if partition.volume.trim().is_empty() {
            return Err(BackupValidationError::SourcePartitionInvalid);
        }
        if self.save_path.trim().is_empty() {
            return Err(BackupValidationError::SavePathRequired);
        }
        if self.name.trim().is_empty() {
            return Err(BackupValidationError::NameRequired);
        }
        if self.format == BackupFormat::Swm && !(512..=8192).contains(&self.swm_split_size_mb) {
            return Err(BackupValidationError::SwmSplitSizeOutOfRange {
                value: self.swm_split_size_mb,
            });
        }
        Ok(())
    }

    /// Converts validated UI intent to the existing PE-compatible configuration model.
    /// This is a pure conversion: it does not write configuration or start a backup.
    pub fn to_backup_config(
        &self,
        partitions: &[BackupPartitionRow],
        wim_engine: u8,
    ) -> Result<BackupConfig, BackupValidationError> {
        self.validate(partitions)?;
        let index = self
            .source_partition
            .ok_or(BackupValidationError::SourcePartitionRequired)?;
        let partition = partitions
            .get(index)
            .ok_or(BackupValidationError::SourcePartitionUnavailable { index })?;
        Ok(BackupConfig {
            save_path: self.save_path.trim().to_owned(),
            name: self.name.trim().to_owned(),
            description: self.description.clone(),
            source_partition: partition.volume.trim().to_owned(),
            incremental: self.incremental,
            format: self.format.to_config_value(),
            swm_split_size: self.swm_split_size_mb,
            wim_engine,
        })
    }
}

/// Presentation-only partition data. The controller retains the authoritative partition model.
#[derive(Debug, Clone)]
pub struct BackupPartitionRow {
    pub volume: String,
    pub total_size: String,
    pub used_size: String,
    pub label: String,
    pub bitlocker: String,
    pub status: String,
    pub has_windows: bool,
    pub is_system_partition: bool,
}

/// Preserves the source-selection behavior of the last egui backup page.
///
/// The desktop selects the current system partition. PE selects a Windows partition only when
/// exactly one candidate exists; with zero or multiple offline systems it deliberately leaves the
/// choice empty so the operator must identify the intended source.
pub fn legacy_default_source_index(
    partitions: &[BackupPartitionRow],
    is_pe_environment: bool,
) -> Option<usize> {
    if !is_pe_environment {
        return partitions
            .iter()
            .position(|partition| partition.is_system_partition);
    }

    let mut candidates = partitions
        .iter()
        .enumerate()
        .filter(|(_, partition)| partition.has_windows)
        .map(|(index, _)| index);
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

pub fn source_warning_text(
    partitions: &[BackupPartitionRow],
    selected: Option<usize>,
    is_pe_environment: bool,
) -> String {
    match selected.and_then(|index| partitions.get(index)) {
        Some(partition) if !partition.has_windows => {
            crate::tr!("所选分区似乎没有 Windows 系统")
        }
        // The route is an implementation detail.  Keep the validation feedback, but do not
        // consume scarce form width with a redundant "backup through PE" annotation.
        Some(partition) if partition.is_system_partition && !is_pe_environment => String::new(),
        Some(_) => crate::tr!("(直接备份)"),
        None => crate::tr!("请选择源分区和备份保存位置。"),
    }
}

#[derive(Clone, Copy)]
pub struct BackupPageHandles {
    pub source_label: HWND,
    pub source_list: HWND,
    pub format_label: HWND,
    pub format: HWND,
    pub format_hint: HWND,
    pub swm_size_label: HWND,
    pub swm_size: HWND,
    pub save_label: HWND,
    pub save_path: HWND,
    pub browse: HWND,
    pub name_label: HWND,
    pub name: HWND,
    pub description_label: HWND,
    pub description: HWND,
    pub incremental: HWND,
    pub warning: HWND,
    pub pe_label: HWND,
    pub pe: HWND,
}

pub struct BackupPage {
    handles: BackupPageHandles,
    pe_count: usize,
    default_timestamp: String,
    generated_name: RefCell<String>,
    generated_description: RefCell<String>,
}

impl BackupPage {
    /// Creates a hidden page. Call `layout`, `apply_theme`, `apply_font`, then `show(true)`.
    pub unsafe fn create(
        parent: HWND,
        partitions: &[BackupPartitionRow],
        pe_labels: &[String],
        initial: &BackupPageState,
        default_timestamp: &str,
    ) -> windows::core::Result<Self> {
        let source_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("选择要备份的分区:"),
            0,
            409,
        )?;
        let source_list = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("SysListView32"),
            w!(""),
            WINDOW_STYLE((WS_CHILD | WS_TABSTOP).0 | LVS_REPORT | LVS_SHOWSELALWAYS),
            0,
            0,
            0,
            0,
            parent,
            HMENU(ID_SOURCE_LIST as isize as *mut _),
            HINSTANCE::default(),
            None,
        )?;
        let _ = SendMessageW(
            source_list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize),
        );

        let format_label = child(parent, w!("STATIC"), &crate::tr!("备份格式:"), 0, 418)?;
        let format = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_FORMAT,
        )?;
        for value in [
            crate::tr!("WIM (推荐)"),
            crate::tr!("ESD (高压缩)"),
            crate::tr!("SWM (分卷)"),
            "GHO (Ghost)".to_owned(),
        ] {
            let value = wide(&value);
            let _ = SendMessageW(
                format,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(value.as_ptr() as isize),
            );
        }
        let _ = SendMessageW(
            format,
            CB_SETCURSEL,
            WPARAM(format_index(initial.format)),
            LPARAM(0),
        );
        let format_hint = child(parent, w!("STATIC"), &format_hint(initial.format), 0, 419)?;
        let swm_size_label = child(parent, w!("STATIC"), &crate::tr!("分卷大小:"), 0, 420)?;
        let swm_size = edit(parent, ID_SWM_SIZE, &initial.swm_split_size_mb.to_string())?;

        let save_label = child(parent, w!("STATIC"), &crate::tr!("保存位置:"), 0, 421)?;
        let save_path = edit(parent, ID_SAVE_PATH, &initial.save_path)?;
        let browse = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("浏览..."),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_BROWSE,
        )?;
        let name_label = child(parent, w!("STATIC"), &crate::tr!("备份名称:"), 0, 422)?;
        let name = edit(parent, ID_NAME, &initial.name)?;
        let description_label = child(parent, w!("STATIC"), &crate::tr!("备份描述:"), 0, 423)?;
        let description = edit(parent, ID_DESCRIPTION, &initial.description)?;
        let incremental = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("增量备份 (追加到现有镜像)"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_INCREMENTAL,
        )?;
        let _ = SendMessageW(
            incremental,
            0x00F1,
            WPARAM(usize::from(initial.incremental)),
            LPARAM(0),
        );
        let warning = child(
            parent,
            w!("STATIC"),
            &crate::tr!("请选择源分区和备份保存位置。"),
            0,
            424,
        )?;
        let pe_label = child(parent, w!("STATIC"), &crate::tr!("PE 环境:"), 0, 426)?;
        let pe = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_PE,
        )?;
        for label in pe_labels {
            let label = wide(label);
            let _ = SendMessageW(pe, CB_ADDSTRING, WPARAM(0), LPARAM(label.as_ptr() as isize));
        }
        if pe_labels.len() == 1 {
            let _ = SendMessageW(pe, CB_SETCURSEL, WPARAM(0), LPARAM(0));
        }

        let page = Self {
            handles: BackupPageHandles {
                source_label,
                source_list,
                format_label,
                format,
                format_hint,
                swm_size_label,
                swm_size,
                save_label,
                save_path,
                browse,
                name_label,
                name,
                description_label,
                description,
                incremental,
                warning,
                pe_label,
                pe,
            },
            pe_count: pe_labels.len(),
            default_timestamp: default_timestamp.to_owned(),
            generated_name: RefCell::new(initial.name.clone()),
            generated_description: RefCell::new(initial.description.clone()),
        };
        page.populate_partitions(partitions, initial.source_partition, true, true);
        page.update_source_warning(
            partitions,
            crate::core::disk::DiskManager::is_pe_environment(),
        );
        page.update_format_controls();
        page.show(false);
        Ok(page)
    }

    pub const fn handles(&self) -> BackupPageHandles {
        self.handles
    }

    pub unsafe fn set_save_path(&self, path: &str, incremental: bool) {
        set_text(self.handles.save_path, path);
        let _ = SendMessageW(
            self.handles.incremental,
            0x00F1,
            WPARAM(usize::from(incremental)),
            LPARAM(0),
        );
    }

    pub unsafe fn selected_pe(&self) -> Option<usize> {
        usize::try_from(SendMessageW(self.handles.pe, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0)
            .ok()
            .filter(|index| *index < self.pe_count)
    }

    pub unsafe fn replace_pe_labels(&mut self, labels: &[String]) {
        let _ = SendMessageW(self.handles.pe, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for label in labels {
            let label = wide(label);
            let _ = SendMessageW(
                self.handles.pe,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(label.as_ptr() as isize),
            );
        }
        self.pe_count = labels.len();
        let selected = if labels.len() == 1 { 0 } else { usize::MAX };
        let _ = SendMessageW(self.handles.pe, CB_SETCURSEL, WPARAM(selected), LPARAM(0));
    }

    pub unsafe fn relocalize(&self) {
        let h = self.handles;
        set_text(h.source_label, &crate::tr!("选择要备份的分区:"));
        set_text(h.format_label, &crate::tr!("备份格式:"));
        set_text(h.swm_size_label, &crate::tr!("分卷大小:"));
        set_text(h.save_label, &crate::tr!("保存位置:"));
        set_text(h.browse, &crate::tr!("浏览..."));
        set_text(h.name_label, &crate::tr!("备份名称:"));
        set_text(h.description_label, &crate::tr!("备份描述:"));
        set_text(h.incremental, &crate::tr!("增量备份 (追加到现有镜像)"));
        set_text(h.pe_label, &crate::tr!("PE 环境:"));

        // Generated defaults follow the selected interface language. Each field is considered
        // independently so editing only the description does not freeze the generated name (or
        // vice versa). Values that no longer equal the previous generated text are user-owned and
        // must never be overwritten by a language switch.
        let (next_name, next_description) = localized_backup_defaults(&self.default_timestamp);
        let current_name = read_text(h.name);
        let current_description = read_text(h.description);
        let previous_name = self.generated_name.borrow().clone();
        let previous_description = self.generated_description.borrow().clone();
        set_text(
            h.name,
            &relocalize_generated_value(current_name, &previous_name, &next_name),
        );
        set_text(
            h.description,
            &relocalize_generated_value(
                current_description,
                &previous_description,
                &next_description,
            ),
        );
        *self.generated_name.borrow_mut() = next_name;
        *self.generated_description.borrow_mut() = next_description;

        let selected = SendMessageW(h.format, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let _ = SendMessageW(h.format, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for value in [
            crate::tr!("WIM (推荐)"),
            crate::tr!("ESD (高压缩)"),
            crate::tr!("SWM (分卷)"),
            "GHO (Ghost)".to_owned(),
        ] {
            let value = wide(&value);
            let _ = SendMessageW(
                h.format,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(value.as_ptr() as isize),
            );
        }
        let _ = SendMessageW(
            h.format,
            CB_SETCURSEL,
            WPARAM(selected.max(0) as usize),
            LPARAM(0),
        );
        self.update_format_controls();

        for (index, title) in [
            crate::tr!("分区卷"),
            crate::tr!("总空间"),
            crate::tr!("已用空间"),
            crate::tr!("卷标"),
            "BitLocker".to_owned(),
            crate::tr!("状态"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut title = wide(&title);
            let mut column = LVCOLUMNW {
                mask: LVCF_TEXT,
                pszText: PWSTR(title.as_mut_ptr()),
                ..Default::default()
            };
            let _ = SendMessageW(
                h.source_list,
                0x1060,
                WPARAM(index),
                LPARAM((&mut column as *mut LVCOLUMNW) as isize),
            );
        }
        let long_state_labels = crate::tr!("未加密").chars().count() > 6;
        let _ = SendMessageW(
            h.source_list,
            0x101E,
            WPARAM(4),
            LPARAM((if long_state_labels { 120 } else { 92 }) as isize),
        );
        let _ = SendMessageW(
            h.source_list,
            0x101E,
            WPARAM(5),
            LPARAM((if long_state_labels { 148 } else { 80 }) as isize),
        );
    }

    /// Refreshes the visible inventory after the controller has matched a prior selection
    /// against the newly enumerated stable partition identity.
    pub unsafe fn replace_partitions(
        &self,
        partitions: &[BackupPartitionRow],
        selected: Option<usize>,
    ) {
        self.populate_partitions(partitions, selected, false, false);
    }

    pub unsafe fn show_pe_selector(&self, visible: bool) {
        // A sole PE is selected automatically and is not a meaningful user choice.
        let command = if visible && self.pe_count > 1 {
            SW_SHOW
        } else {
            SW_HIDE
        };
        let _ = ShowWindow(self.handles.pe_label, command);
        let _ = ShowWindow(self.handles.pe, command);
    }

    /// Refreshes the legacy source warning after selection or inventory changes.
    ///
    /// Validation and PE-availability errors remain controller-owned and may overwrite this text.
    pub unsafe fn update_source_warning(
        &self,
        partitions: &[BackupPartitionRow],
        is_pe_environment: bool,
    ) {
        let selected = self.read_state().source_partition;
        let message = source_warning_text(partitions, selected, is_pe_environment);
        set_text(self.handles.warning, &message);
    }

    /// Positions the page within the caller's content rectangle using logical 96-DPI units.
    pub unsafe fn layout(&self, left: i32, top: i32, width: i32, dpi: u32) {
        let s = |value: i32| value * dpi as i32 / 96;
        let h = self.handles;
        let width = width.max(0);
        let translated_labels_are_long = [
            crate::tr!("备份格式:"),
            crate::tr!("保存位置:"),
            crate::tr!("备份名称:"),
            crate::tr!("备份描述:"),
        ]
        .iter()
        .any(|label| label.chars().count() > 7);
        let label_width = s(if translated_labels_are_long { 116 } else { 76 }).min(width / 3);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let row_height = metrics.field_height.max(metrics.button_height);
        let table_top = top + s(26);
        let table_height = s(132);

        move_control(h.source_label, left, top, s(180).min(width), s(20));
        move_control(h.source_list, left, table_top, width, table_height);
        for (column, column_width) in backup_column_widths(width, dpi).into_iter().enumerate() {
            let _ = SendMessageW(
                h.source_list,
                0x101E, // LVM_SETCOLUMNWIDTH
                WPARAM(column),
                LPARAM(column_width as isize),
            );
        }

        let format_top = table_top + table_height + s(12);
        let format_closed_height = combo_closed_height(h.format, metrics.field_height);
        let format_row_height = format_closed_height.max(metrics.field_height);
        let label_y = centered_control_y_ceil(format_top, format_row_height, metrics.label_height);
        move_control(
            h.format_label,
            left,
            label_y,
            label_width,
            metrics.label_height,
        );
        let format_width = s(132).min((width - label_width).max(0));
        move_control(
            h.format,
            left + label_width,
            centered_control_y_ceil(format_top, format_row_height, format_closed_height),
            format_width,
            s(180),
        );
        let detail_x = (label_width + format_width + s(10)).min(width);
        move_control(
            h.format_hint,
            left + detail_x,
            label_y,
            width - detail_x,
            metrics.label_height,
        );
        let swm_label_width = s(72).min((width - detail_x) / 2);
        move_control(
            h.swm_size_label,
            left + detail_x,
            label_y,
            swm_label_width,
            metrics.label_height,
        );
        move_control(
            h.swm_size,
            left + detail_x + swm_label_width,
            centered_control_y_ceil(format_top, format_row_height, row_height),
            width - detail_x - swm_label_width,
            row_height,
        );

        let save_top = format_top + format_row_height + s(10);
        let browse_width = s(76).min(width / 4);
        move_control(h.save_label, left, save_top + s(3), label_width, s(20));
        move_control(
            h.save_path,
            left + label_width,
            save_top,
            width - label_width - browse_width - s(8),
            row_height,
        );
        move_control(
            h.browse,
            left + width - browse_width,
            save_top,
            browse_width,
            row_height,
        );

        let name_top = save_top + s(32);
        move_control(h.name_label, left, name_top + s(3), label_width, s(20));
        move_control(
            h.name,
            left + label_width,
            name_top,
            width - label_width,
            row_height,
        );
        let description_top = name_top + s(32);
        move_control(
            h.description_label,
            left,
            description_top + s(3),
            label_width,
            s(20),
        );
        move_control(
            h.description,
            left + label_width,
            description_top,
            width - label_width,
            row_height,
        );
        let options_top = description_top + s(32);
        let incremental_width = s(230).min(width / 2);
        let options_gap = s(8).min((width - incremental_width).max(0));
        move_control(
            h.incremental,
            left,
            options_top,
            incremental_width,
            row_height,
        );
        move_control(
            h.warning,
            left + incremental_width + options_gap,
            options_top + s(3),
            width - incremental_width - options_gap,
            s(20),
        );
        let pe_top = options_top + s(32);
        let pe_closed_height = combo_closed_height(h.pe, metrics.field_height);
        let pe_row_height = pe_closed_height.max(metrics.field_height);
        move_control(
            h.pe_label,
            left,
            centered_control_y_ceil(pe_top, pe_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
        );
        move_control(
            h.pe,
            left + label_width,
            centered_control_y_ceil(pe_top, pe_row_height, pe_closed_height),
            (width - label_width).min(s(320)).max(0),
            s(180),
        );
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for control in self.all_controls() {
            let _ = ShowWindow(control, command);
        }
        if visible {
            self.update_format_controls();
            if self.pe_count <= 1 {
                let _ = ShowWindow(self.handles.pe_label, SW_HIDE);
                let _ = ShowWindow(self.handles.pe, SW_HIDE);
            }
        }
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        let _ = apply_list_view_theme(self.handles.source_list, palette);
        for control in [self.handles.incremental, self.handles.browse] {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        for control in [
            self.handles.format,
            self.handles.swm_size,
            self.handles.save_path,
            self.handles.name,
            self.handles.description,
            self.handles.pe,
        ] {
            apply_control_theme(control, palette, NativeControlKind::Field);
        }
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(
                self.handles.source_list,
                message,
                WPARAM(0),
                LPARAM(color.0 as isize),
            );
        }
    }

    pub unsafe fn apply_font(&self, font: windows::Win32::Graphics::Gdi::HFONT) {
        for control in self.all_controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    /// Refreshes format-specific text and the SWM size editor after `CBN_SELCHANGE`.
    pub unsafe fn update_format_controls(&self) {
        let format = self.selected_format();
        set_text(self.handles.format_hint, &format_hint(format));
        let swm = format == BackupFormat::Swm;
        let _ = ShowWindow(
            self.handles.format_hint,
            if swm { SW_HIDE } else { SW_SHOW },
        );
        let _ = ShowWindow(
            self.handles.swm_size_label,
            if swm { SW_SHOW } else { SW_HIDE },
        );
        let _ = ShowWindow(self.handles.swm_size, if swm { SW_SHOW } else { SW_HIDE });
    }

    pub unsafe fn read_state(&self) -> BackupPageState {
        let selected = SendMessageW(
            self.handles.source_list,
            LVM_GETNEXTITEM,
            WPARAM(usize::MAX),
            LPARAM(LVIS_SELECTED.0 as isize),
        )
        .0;
        BackupPageState {
            source_partition: (selected >= 0).then_some(selected as usize),
            format: self.selected_format(),
            swm_split_size_mb: read_text(self.handles.swm_size)
                .trim()
                .parse::<u32>()
                .unwrap_or(0),
            save_path: read_text(self.handles.save_path),
            name: read_text(self.handles.name),
            description: read_text(self.handles.description),
            incremental: SendMessageW(self.handles.incremental, BM_GETCHECK, WPARAM(0), LPARAM(0))
                .0
                == 1,
        }
    }

    pub unsafe fn set_enabled(&self, enabled: bool) {
        for control in [
            self.handles.source_list,
            self.handles.format,
            self.handles.swm_size,
            self.handles.save_path,
            self.handles.browse,
            self.handles.name,
            self.handles.description,
            self.handles.incremental,
            self.handles.pe,
        ] {
            let _ = EnableWindow(control, enabled);
        }
    }

    pub unsafe fn selected_format(&self) -> BackupFormat {
        match SendMessageW(self.handles.format, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 {
            1 => BackupFormat::Esd,
            2 => BackupFormat::Swm,
            3 => BackupFormat::Gho,
            _ => BackupFormat::Wim,
        }
    }

    unsafe fn populate_partitions(
        &self,
        partitions: &[BackupPartitionRow],
        selected: Option<usize>,
        add_columns: bool,
        select_system_default: bool,
    ) {
        if add_columns {
            let long_state_labels = crate::tr!("未加密").chars().count() > 6;
            for (index, (title, width)) in [
                (crate::tr!("分区卷"), 120),
                (crate::tr!("总空间"), 82),
                (crate::tr!("已用空间"), 82),
                (crate::tr!("卷标"), 94),
                (
                    "BitLocker".to_owned(),
                    if long_state_labels { 120 } else { 92 },
                ),
                (crate::tr!("状态"), if long_state_labels { 148 } else { 80 }),
            ]
            .into_iter()
            .enumerate()
            {
                let mut title = wide(&title);
                let mut column = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH,
                    cx: width,
                    pszText: PWSTR(title.as_mut_ptr()),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    self.handles.source_list,
                    LVM_INSERTCOLUMNW,
                    WPARAM(index),
                    LPARAM((&mut column as *mut LVCOLUMNW) as isize),
                );
            }
        }
        let _ = SendMessageW(
            self.handles.source_list,
            LVM_DELETEALLITEMS,
            WPARAM(0),
            LPARAM(0),
        );
        for (row, partition) in partitions.iter().enumerate() {
            let volume = if partition.is_system_partition {
                crate::tr!("{} (当前系统)", partition.volume)
            } else {
                partition.volume.clone()
            };
            for (column, value) in [
                volume,
                partition.total_size.clone(),
                partition.used_size.clone(),
                partition.label.clone(),
                partition.bitlocker.clone(),
                partition.status.clone(),
            ]
            .into_iter()
            .enumerate()
            {
                let mut value = wide(value);
                let mut item = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row as i32,
                    iSubItem: column as i32,
                    pszText: PWSTR(value.as_mut_ptr()),
                    ..Default::default()
                };
                let message = if column == 0 { LVM_INSERTITEMW } else { 0x104C };
                let _ = SendMessageW(
                    self.handles.source_list,
                    message,
                    WPARAM(0),
                    LPARAM((&mut item as *mut LVITEMW) as isize),
                );
            }
        }
        let selected = selected.or_else(|| {
            select_system_default
                .then(|| {
                    legacy_default_source_index(
                        partitions,
                        crate::core::disk::DiskManager::is_pe_environment(),
                    )
                })
                .flatten()
        });
        if let Some(row) = selected.filter(|row| *row < partitions.len()) {
            let mut item = LVITEMW {
                stateMask: LVIS_SELECTED,
                state: LVIS_SELECTED,
                iItem: row as i32,
                ..Default::default()
            };
            let _ = SendMessageW(
                self.handles.source_list,
                LVM_SETITEMSTATE,
                WPARAM(row),
                LPARAM((&mut item as *mut LVITEMW) as isize),
            );
        }
    }

    fn all_controls(&self) -> [HWND; 18] {
        let h = self.handles;
        [
            h.source_label,
            h.source_list,
            h.format_label,
            h.format,
            h.format_hint,
            h.swm_size_label,
            h.swm_size,
            h.save_label,
            h.save_path,
            h.browse,
            h.name_label,
            h.name,
            h.description_label,
            h.description,
            h.incremental,
            h.warning,
            h.pe_label,
            h.pe,
        ]
    }
}

fn format_index(format: BackupFormat) -> usize {
    match format {
        BackupFormat::Wim => 0,
        BackupFormat::Esd => 1,
        BackupFormat::Swm => 2,
        BackupFormat::Gho => 3,
    }
}

fn format_hint(format: BackupFormat) -> String {
    match format {
        BackupFormat::Wim => crate::tr!("标准 WIM 格式，兼容性好"),
        BackupFormat::Esd => crate::tr!("高压缩率，文件体积更小"),
        BackupFormat::Swm => String::new(),
        BackupFormat::Gho => crate::tr!("需要 Ghost 工具支持"),
    }
}

/// Keeps the backup inventory readable at normal window sizes while retaining horizontal
/// scrolling as the safe fallback at genuinely narrow widths.  The old fixed pixel widths did
/// not scale with DPI and left a large unused area after the status column.
fn backup_column_widths(table_width: i32, dpi: u32) -> [i32; 6] {
    let scale = |value: i32| value * dpi as i32 / 96;
    let usable = (table_width - scale(18)).max(0); // reserve the vertical scrollbar/border
    let minimums = [
        scale(132),
        scale(104),
        scale(104),
        scale(96),
        scale(108),
        scale(104),
    ];
    let minimum_total: i32 = minimums.iter().sum();
    if usable <= minimum_total {
        return minimums;
    }

    let weights = [18, 14, 14, 13, 14, 27];
    let extra = usable - minimum_total;
    let mut widths = minimums;
    let mut distributed = 0;
    for index in 0..widths.len() - 1 {
        let addition = extra * weights[index] / 100;
        widths[index] += addition;
        distributed += addition;
    }
    widths[5] += extra - distributed;
    widths
}

unsafe fn edit(parent: HWND, id: u16, text: &str) -> windows::core::Result<HWND> {
    let numeric = if id == ID_SWM_SIZE { ES_NUMBER } else { 0 };
    child(
        parent,
        w!("EDIT"),
        text,
        WS_TABSTOP.0 as i32 | ES_AUTOHSCROLL | numeric,
        id,
    )
}

unsafe fn read_text(hwnd: HWND) -> String {
    let length = GetWindowTextLengthW(hwnd);
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0_u16; length as usize + 1];
    let copied = GetWindowTextW(hwnd, &mut buffer);
    String::from_utf16_lossy(&buffer[..copied as usize])
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, PCWSTR(text.as_ptr()));
}

unsafe fn move_control(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = MoveWindow(hwnd, x, y, width.max(0), height.max(0), true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partition() -> BackupPartitionRow {
        BackupPartitionRow {
            volume: "C:".to_owned(),
            total_size: "100 GB".to_owned(),
            used_size: "40 GB".to_owned(),
            label: "Windows".to_owned(),
            bitlocker: "未加密".to_owned(),
            status: "已有系统".to_owned(),
            has_windows: true,
            is_system_partition: true,
        }
    }

    fn valid_state() -> BackupPageState {
        BackupPageState {
            source_partition: Some(0),
            save_path: " D:\\backup.wim ".to_owned(),
            name: " System Backup ".to_owned(),
            description: "Created by LetRecovery".to_owned(),
            incremental: true,
            ..BackupPageState::default()
        }
    }

    #[test]
    fn backup_defaults_match_the_legacy_page() {
        let state = BackupPageState::default();
        assert_eq!(state.format, BackupFormat::Wim);
        assert_eq!(state.swm_split_size_mb, 4096);
        assert!(!state.incremental);
    }

    #[test]
    fn generated_backup_fields_keep_one_timestamp_and_relocalize_independently() {
        let timestamp = "20260714_174715";
        let (name, description) = localized_backup_defaults(timestamp);
        assert!(name.ends_with(timestamp));

        assert_eq!(
            relocalize_generated_value(name.clone(), &name, "System Backup_20260714_174715"),
            "System Backup_20260714_174715"
        );
        assert_eq!(
            relocalize_generated_value(
                "My backup".to_owned(),
                &name,
                "System Backup_20260714_174715"
            ),
            "My backup"
        );
        assert_eq!(
            relocalize_generated_value(
                "My description".to_owned(),
                &description,
                "System backup created with LetRecovery"
            ),
            "My description"
        );
    }

    #[test]
    fn browse_button_uses_shared_owner_draw_pipeline() {
        let style = BS_OWNERDRAW | WS_TABSTOP.0 as i32;
        assert_ne!(style & BS_OWNERDRAW, 0);
    }

    #[test]
    fn every_format_has_a_stable_combo_index() {
        assert_eq!(format_index(BackupFormat::Wim), 0);
        assert_eq!(format_index(BackupFormat::Esd), 1);
        assert_eq!(format_index(BackupFormat::Swm), 2);
        assert_eq!(format_index(BackupFormat::Gho), 3);
    }

    #[test]
    fn backup_columns_use_the_available_width_without_squeezing_numeric_fields() {
        let widths = backup_column_widths(926, 96);
        assert!(widths[0] >= 132);
        assert!(widths[1] >= 104);
        assert!(widths[2] >= 104);
        assert_eq!(widths.iter().sum::<i32>(), 908);

        let narrow = backup_column_widths(500, 96);
        assert_eq!(narrow, [132, 104, 104, 96, 108, 104]);
    }

    #[test]
    fn validation_reports_each_required_field() {
        let partitions = [partition()];
        let mut state = valid_state();
        state.source_partition = None;
        assert_eq!(
            state.validate(&partitions),
            Err(BackupValidationError::SourcePartitionRequired)
        );

        state = valid_state();
        state.source_partition = Some(1);
        assert_eq!(
            state.validate(&partitions),
            Err(BackupValidationError::SourcePartitionUnavailable { index: 1 })
        );

        state = valid_state();
        state.save_path = "   ".to_owned();
        assert_eq!(
            state.validate(&partitions),
            Err(BackupValidationError::SavePathRequired)
        );

        state = valid_state();
        state.name.clear();
        assert_eq!(
            state.validate(&partitions),
            Err(BackupValidationError::NameRequired)
        );
    }

    #[test]
    fn swm_split_size_is_validated_only_for_swm() {
        let partitions = [partition()];
        let mut state = valid_state();
        state.format = BackupFormat::Swm;
        state.swm_split_size_mb = 511;
        assert_eq!(
            state.validate(&partitions),
            Err(BackupValidationError::SwmSplitSizeOutOfRange { value: 511 })
        );
        state.swm_split_size_mb = 8192;
        assert!(state.validate(&partitions).is_ok());

        state.format = BackupFormat::Wim;
        state.swm_split_size_mb = 0;
        assert!(state.validate(&partitions).is_ok());
    }

    #[test]
    fn conversion_preserves_format_incremental_and_legacy_config_fields() {
        let partitions = [partition()];
        let mut state = valid_state();
        state.format = BackupFormat::Esd;
        let config = state.to_backup_config(&partitions, 1).unwrap();
        assert_eq!(config.source_partition, "C:");
        assert_eq!(config.save_path, "D:\\backup.wim");
        assert_eq!(config.name, "System Backup");
        assert_eq!(config.description, "Created by LetRecovery");
        assert!(config.incremental);
        assert_eq!(config.format, 1);
        assert_eq!(config.swm_split_size, 4096);
        assert_eq!(config.wim_engine, 1);
    }

    #[test]
    fn desktop_defaults_to_current_system_partition() {
        let mut rows = vec![partition(), partition()];
        rows[0].is_system_partition = false;
        rows[1].is_system_partition = true;
        assert_eq!(legacy_default_source_index(&rows, false), Some(1));
    }

    #[test]
    fn pe_defaults_only_when_exactly_one_windows_partition_exists() {
        let mut data = partition();
        data.has_windows = false;
        data.is_system_partition = false;
        let mut windows = partition();
        windows.is_system_partition = false;
        let rows = vec![data.clone(), windows.clone()];
        assert_eq!(legacy_default_source_index(&rows, true), Some(1));

        assert_eq!(legacy_default_source_index(&[data.clone()], true), None);
        assert_eq!(
            legacy_default_source_index(&[windows.clone(), windows], true),
            None
        );
    }

    #[test]
    fn source_warning_preserves_validation_without_redundant_pe_route_text() {
        let mut data = partition();
        data.has_windows = false;
        data.is_system_partition = false;
        assert_eq!(
            source_warning_text(&[data], Some(0), false),
            crate::tr!("所选分区似乎没有 Windows 系统")
        );

        assert!(source_warning_text(&[partition()], Some(0), false).is_empty());
        assert_eq!(
            source_warning_text(&[partition()], Some(0), true),
            crate::tr!("(直接备份)")
        );
    }
}
