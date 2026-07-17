//! Native hardware-information and about pages.
//!
//! Hardware values are presented as a responsive category/item/value report ListView;
//! the page also retains a complete tab-separated representation for copying. The about page
//! exposes the existing persisted language, logging, easy-mode, download-connection,
//! image-engine, and advanced-mode preferences. Link and settings controls emit intents; this
//! module never starts a browser or opens filesystem locations.

use std::cell::RefCell;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, BS_AUTOCHECKBOX, BS_OWNERDRAW,
    CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, SW_HIDE, SW_SHOW,
    WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use super::download::PageRect;
use crate::native_ui::controls::{child, wide};
use crate::native_ui::layout::{centered_control_y_ceil, measure_text, LayoutMetrics};
use crate::native_ui::theme::{
    apply_control_theme, apply_list_view_theme, combo_closed_height, NativeControlKind, Palette,
};

pub const ID_HARDWARE_SAVE: u16 = 5_200;
const ID_HARDWARE_REPORT: u16 = 5_201;
const ID_ABOUT_VERSION: u16 = 5_210;
const ID_ABOUT_DESCRIPTION: u16 = 5_211;
const ID_FIRST_LINK: u16 = 5_220;
const ID_ABOUT_EASY_MODE: u16 = 5_215;
const ID_FIRST_ABOUT_ACTION: u16 = 5_240;
const ID_ABOUT_LANGUAGE: u16 = 5_250;
const ID_ABOUT_REFRESH_LANGUAGES: u16 = 5_251;
const ID_ABOUT_LOGGING: u16 = 5_252;
const ID_ABOUT_WIM_ENGINE: u16 = 5_253;
const ID_ABOUT_ADVANCED: u16 = 5_254;
const ID_ABOUT_DOWNLOAD_THREADS: u16 = 5_259;
const ID_ABOUT_EXPERIMENTAL_MICA: u16 = 5_261;
const DOWNLOAD_THREAD_OPTIONS: [u8; 3] = [8, 16, 32];
// Unlike SS_LEFT (zero), this stock STATIC style never wraps a single-line settings label.  A
// clipped second line was the source of the short vertical strokes below English captions.
const SS_LEFTNOWORDWRAP_STYLE: i32 = 0x0000_000C;

fn settings_label_width(measured_widths: &[i32], available_width: i32, dpi: u32) -> i32 {
    let padding = 8 * dpi.max(1) as i32 / 96;
    measured_widths
        .iter()
        .copied()
        .max()
        .unwrap_or_default()
        .saturating_add(padding)
        .min((available_width.max(0) / 3).max(0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoIntent {
    SaveHardwareText,
    OpenLink(AboutLink),
    SelectLanguage,
    RefreshLanguages,
    ToggleEasyMode,
    ToggleLogging,
    SelectWimEngine,
    SelectDownloadThreads,
    ToggleAdvancedOptions,
    ToggleExperimentalMica,
    OpenLogDirectory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AboutAction {
    OpenLogDirectory,
}

impl AboutAction {
    pub const ALL: [Self; 1] = [Self::OpenLogDirectory];

    const fn command_id(self) -> u16 {
        ID_FIRST_ABOUT_ACTION + self as u16
    }

    fn label(self) -> String {
        match self {
            Self::OpenLogDirectory => crate::tr!("打开日志目录"),
        }
    }

    const fn intent(self) -> InfoIntent {
        match self {
            Self::OpenLogDirectory => InfoIntent::OpenLogDirectory,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AboutLink {
    ProjectHomepage,
    Documentation,
    License,
}

impl AboutLink {
    pub const ALL: [Self; 3] = [Self::ProjectHomepage, Self::Documentation, Self::License];

    const fn command_id(self) -> u16 {
        ID_FIRST_LINK + self as u16
    }
}

pub struct HardwareLabels<'a> {
    pub introduction: &'a str,
    pub loading: &'a str,
    pub save: &'a str,
}

pub struct AboutLabels<'a> {
    pub product_name: &'a str,
    pub version_label: &'a str,
    pub version: &'a str,
    pub description: &'a str,
    /// Button captions in `AboutLink::ALL` order.
    pub link_labels: [&'a str; 3],
    pub easy_mode: &'a str,
    pub easy_mode_enabled: bool,
    pub easy_mode_available: bool,
    pub log_enabled: bool,
    pub wim_engine: u8,
    pub download_threads: u8,
    pub advanced_options_enabled: bool,
    pub experimental_mica_enabled: bool,
}

pub struct HardwareInfoPage {
    pub introduction: HWND,
    pub report: HWND,
    pub save: HWND,
    rows: RefCell<Vec<HardwareInfoRow>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareInfoRow {
    pub category: String,
    pub item: String,
    pub value: String,
}

impl HardwareInfoRow {
    fn new(category: impl Into<String>, item: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            item: item.into(),
            value: value.into(),
        }
    }
}

impl HardwareInfoPage {
    pub unsafe fn create(
        parent: HWND,
        font: HFONT,
        labels: &HardwareLabels<'_>,
    ) -> windows::core::Result<Self> {
        let introduction = child(parent, w!("STATIC"), labels.introduction, 0, 5_202)?;
        let report = child(
            parent,
            w!("SysListView32"),
            "",
            (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
            ID_HARDWARE_REPORT,
        )?;
        let _ = SendMessageW(
            report,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
        );
        insert_hardware_columns(report);
        let save = child(
            parent,
            w!("BUTTON"),
            labels.save,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_HARDWARE_SAVE,
        )?;
        let page = Self {
            introduction,
            report,
            save,
            rows: RefCell::new(Vec::new()),
        };
        page.set_rows(vec![HardwareInfoRow::new(
            crate::tr!("状态"),
            crate::tr!("硬件信息"),
            labels.loading,
        )]);
        page.apply_font(font);
        page.show(false);
        Ok(page)
    }

    pub unsafe fn set_rows(&self, rows: Vec<HardwareInfoRow>) {
        let _ = SendMessageW(self.report, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        for (index, row) in rows.iter().enumerate() {
            insert_hardware_item(self.report, index as i32, 0, &row.category);
            insert_hardware_item(self.report, index as i32, 1, &row.item);
            insert_hardware_item(self.report, index as i32, 2, &row.value);
        }
        *self.rows.borrow_mut() = rows;
    }

    /// Refreshes the controls whose captions are owned by this page.
    ///
    /// Callers must rebuild the report rows with [`hardware_info_rows`] after a language
    /// switch because their translated category and item strings are part of the row data.
    pub unsafe fn relocalize(&self, labels: &HardwareLabels<'_>) {
        set_text(self.introduction, labels.introduction);
        set_text(self.save, labels.save);
        update_hardware_columns(self.report);
    }

    pub fn report_text(&self) -> String {
        format_hardware_report(&self.rows.borrow())
    }

    pub fn command_intent(command_id: u16) -> Option<InfoIntent> {
        (command_id == ID_HARDWARE_SAVE).then_some(InfoIntent::SaveHardwareText)
    }

    pub unsafe fn layout(&self, rect: PageRect, dpi: u32) {
        let s = |value: i32| value * dpi as i32 / 96;
        let _ = MoveWindow(
            self.introduction,
            rect.x,
            rect.y + s(5),
            rect.width,
            s(22),
            true,
        );
        // Save is exposed in the stable bottom command bar by the main window. Keeping it out of
        // the page body gives the hardware table the full remaining height.
        let _ = ShowWindow(self.save, SW_HIDE);
        let report_y = rect.y + s(34);
        let _ = MoveWindow(
            self.report,
            rect.x,
            report_y,
            rect.width,
            (rect.y + rect.height - report_y).max(s(100)),
            true,
        );
        let widths = hardware_column_widths(rect.width, dpi);
        for (index, width) in widths.into_iter().enumerate() {
            let _ = SendMessageW(
                self.report,
                LVM_SETCOLUMNWIDTH,
                WPARAM(index),
                LPARAM(width as isize),
            );
        }
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for hwnd in [self.introduction, self.report] {
            let _ = ShowWindow(hwnd, command);
        }
        let _ = ShowWindow(self.save, SW_HIDE);
    }

    pub unsafe fn apply_font(&self, font: HFONT) {
        for hwnd in [self.introduction, self.report, self.save] {
            let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        let _ = apply_list_view_theme(self.report, palette);
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(self.report, message, WPARAM(0), LPARAM(color.0 as isize));
        }
        apply_control_theme(self.save, palette, NativeControlKind::General);
    }
}

/// Formats the report as human-readable sections rather than repeating the ListView's three
/// columns on every exported line. The UI deliberately leaves repeated category cells blank;
/// this formatter carries the active category forward and emits it once as a section heading.
/// Embedded line breaks are retained as indented continuation lines, so a device description or
/// diagnostic value can never glue the next item onto the same physical line.
fn format_hardware_report(rows: &[HardwareInfoRow]) -> String {
    let mut output = String::new();
    let mut active_category = String::new();

    for row in rows {
        let category = normalize_report_cell(&row.category);
        if !category.is_empty() && category != active_category {
            if !output.is_empty() {
                output.push_str("\r\n");
            }
            active_category.clone_from(&category);
            output.push('[');
            output.push_str(&active_category);
            output.push_str("]\r\n");
        }

        let item = normalize_report_cell(&row.item);
        let value = normalize_report_cell(&row.value);
        if item.is_empty() && value.is_empty() {
            continue;
        }

        let mut value_lines = value.split('\n');
        let first_value = value_lines.next().unwrap_or_default();
        if item.is_empty() {
            output.push_str(first_value);
        } else {
            output.push_str(&item);
            output.push_str(": ");
            output.push_str(first_value);
        }
        output.push_str("\r\n");
        for continuation in value_lines {
            output.push_str("    ");
            output.push_str(continuation);
            output.push_str("\r\n");
        }
    }

    output.trim_end_matches(['\r', '\n']).to_owned()
}

fn normalize_report_cell(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', " ")
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

fn hardware_column_widths(width: i32, dpi: u32) -> [i32; 3] {
    let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
    let width = width.max(0);
    // Category values are deliberately short section names. Keep this column narrow and
    // reserve the report's flexible space for model names, device paths and other long values.
    // The DPI-scaled minima still leave the longest English category readable; when the page
    // is narrower than all three minima, the report scrolls horizontally instead of clipping.
    let category = (width * 12 / 100).clamp(scale(88), scale(120));
    let item = (width * 22 / 100).clamp(scale(128), scale(200));
    let value = (width - category - item - scale(4)).max(scale(300));
    [category, item, value]
}

unsafe fn insert_hardware_columns(list: HWND) {
    for (index, (title, width)) in [
        (crate::tr!("类别"), 120),
        (crate::tr!("项目"), 160),
        (crate::tr!("值"), 420),
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

unsafe fn update_hardware_columns(list: HWND) {
    for (index, title) in [crate::tr!("类别"), crate::tr!("项目"), crate::tr!("值")]
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
            LVM_SETCOLUMNW,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn insert_hardware_item(list: HWND, row: i32, column: i32, value: &str) {
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
        0x104C // LVM_SETITEMTEXTW
    };
    let _ = SendMessageW(
        list,
        message,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

pub fn hardware_info_rows(
    info: &crate::core::hardware_info::HardwareInfo,
    system: Option<&crate::core::system_info::SystemInfo>,
) -> Vec<HardwareInfoRow> {
    let mut rows = Vec::new();
    let mut previous_category = String::new();
    let mut push = |category: &str, item: &str, value: String| {
        // A report-style ListView has no native group header in this layout. Repeating the
        // category on every row makes the first two columns read like "System System ...".
        // Keep the category only on the first row of each contiguous section so the item and
        // value columns remain easy to scan without changing their order or content.
        let displayed_category = if previous_category == category {
            String::new()
        } else {
            previous_category.clear();
            previous_category.push_str(category);
            category.to_owned()
        };
        rows.push(HardwareInfoRow::new(
            displayed_category,
            item,
            display_value(value),
        ))
    };

    let system_category = crate::tr!("系统");
    push(
        &system_category,
        &crate::tr!("操作系统"),
        info.os.name.clone(),
    );
    push(
        &system_category,
        &crate::tr!("版本"),
        info.os.version.clone(),
    );
    push(
        &system_category,
        &crate::tr!("版本号"),
        info.os.build_number.clone(),
    );
    push(
        &system_category,
        &crate::tr!("系统架构"),
        localized_architecture(&info.os.architecture),
    );
    push(
        &system_category,
        &crate::tr!("计算机名"),
        info.computer_name.clone(),
    );
    if let Some(system) = system {
        push(
            &system_category,
            &crate::tr!("启动模式"),
            system.boot_mode.to_string(),
        );
        push(
            &system_category,
            "TPM",
            if system.tpm_enabled {
                crate::tr!("已开启 v{}", system.tpm_version)
            } else {
                crate::tr!("未开启")
            },
        );
        push(
            &system_category,
            &crate::tr!("安全启动"),
            yes_no(system.secure_boot),
        );
    }
    push(
        &system_category,
        "BitLocker",
        bitlocker_status_text(&info.system_bitlocker_status),
    );

    let device = crate::tr!("设备");
    push(
        &device,
        &crate::tr!("制造商"),
        crate::core::hardware_info::beautify_manufacturer_name(&info.computer_manufacturer),
    );
    push(&device, &crate::tr!("型号"), info.computer_model.clone());
    push(
        &device,
        &crate::tr!("设备编号"),
        info.system_serial_number.clone(),
    );
    push(
        &device,
        &crate::tr!("设备类型"),
        info.device_type.to_string(),
    );

    let cpu = crate::tr!("处理器");
    push(&cpu, &crate::tr!("型号"), info.cpu.name.clone());
    push(&cpu, &crate::tr!("制造商"), info.cpu.manufacturer.clone());
    push(&cpu, &crate::tr!("架构"), info.cpu.architecture.clone());
    push(&cpu, &crate::tr!("核心数"), info.cpu.cores.to_string());
    push(
        &cpu,
        &crate::tr!("逻辑处理器"),
        info.cpu.logical_processors.to_string(),
    );
    push(
        &cpu,
        &crate::tr!("最大频率"),
        format!("{} MHz", info.cpu.max_clock_speed),
    );
    push(&cpu, &crate::tr!("AI 支持"), yes_no(info.cpu.supports_ai));

    let memory = crate::tr!("内存");
    push(
        &memory,
        &crate::tr!("总容量"),
        format_gib(info.memory.total_physical),
    );
    push(
        &memory,
        &crate::tr!("可用容量"),
        format_gib(info.memory.available_physical),
    );
    push(
        &memory,
        &crate::tr!("使用率"),
        format!("{}%", info.memory.memory_load),
    );
    push(
        &memory,
        &crate::tr!("插槽数"),
        info.memory.slot_count.to_string(),
    );
    for (index, stick) in info.memory.sticks.iter().enumerate() {
        push(
            &memory,
            &crate::tr!("内存条 {}", index + 1),
            format!(
                "{} | {} | {} | {} MHz | {}",
                crate::core::hardware_info::beautify_memory_manufacturer(&stick.manufacturer),
                display_value(stick.part_number.clone()),
                format_gib(stick.capacity),
                stick.speed,
                display_value(stick.device_locator.clone())
            ),
        );
    }

    let board = crate::tr!("主板与 BIOS");
    push(
        &board,
        &crate::tr!("主板制造商"),
        info.motherboard.manufacturer.clone(),
    );
    push(
        &board,
        &crate::tr!("主板型号"),
        info.motherboard.product.clone(),
    );
    push(
        &board,
        &crate::tr!("主板序列号"),
        info.motherboard.serial_number.clone(),
    );
    push(&board, &crate::tr!("BIOS 版本"), info.bios.version.clone());
    push(
        &board,
        &crate::tr!("BIOS 日期"),
        info.bios.release_date.clone(),
    );

    let graphics = crate::tr!("显卡");
    for (index, gpu) in info.gpus.iter().enumerate() {
        let prefix = crate::tr!("显卡 {}", index + 1);
        push(
            &graphics,
            &prefix,
            crate::core::hardware_info::beautify_gpu_name(&gpu.name),
        );
        push(
            &graphics,
            &crate::tr!("{} 驱动", prefix),
            format!("{} ({})", gpu.driver_version, gpu.driver_date),
        );
        push(
            &graphics,
            &crate::tr!("{} 显存", prefix),
            format_gib(gpu.video_memory),
        );
        push(
            &graphics,
            &crate::tr!("{} 显示模式", prefix),
            format!("{} @ {} Hz", gpu.current_resolution, gpu.refresh_rate),
        );
    }

    let storage = crate::tr!("存储");
    for disk in &info.disks {
        let prefix = crate::tr!("磁盘 {}", disk.disk_index);
        push(&storage, &prefix, disk.model.clone());
        push(
            &storage,
            &crate::tr!("{} 容量", prefix),
            format_gib(disk.size),
        );
        push(
            &storage,
            &crate::tr!("{} 接口与分区表", prefix),
            format!("{} | {}", disk.interface_type, disk.partition_style),
        );
        push(
            &storage,
            &crate::tr!("{} 类型", prefix),
            if disk.is_ssd {
                crate::tr!("固态硬盘")
            } else {
                display_value(disk.media_type.clone())
            },
        );
        push(
            &storage,
            &crate::tr!("{} 序列号", prefix),
            disk.serial_number.clone(),
        );
    }

    let network = crate::tr!("网络");
    for (index, adapter) in info.network_adapters.iter().enumerate() {
        let prefix = crate::tr!("网卡 {}", index + 1);
        push(&network, &prefix, adapter.description.clone());
        push(
            &network,
            &crate::tr!("{} MAC", prefix),
            adapter.mac_address.clone(),
        );
        push(
            &network,
            &crate::tr!("{} IP", prefix),
            adapter.ip_addresses.join(", "),
        );
        push(
            &network,
            &crate::tr!("{} 状态", prefix),
            format!(
                "{} | {} Mbps",
                localized_network_status(&adapter.status),
                adapter.speed / 1_000_000
            ),
        );
    }

    if let Some(battery) = &info.battery {
        let category = crate::tr!("电池");
        push(
            &category,
            &crate::tr!("当前电量"),
            format!("{}%", battery.charge_percent),
        );
        push(
            &category,
            &crate::tr!("充电状态"),
            if battery.is_charging {
                crate::tr!("充电中")
            } else if battery.is_ac_connected {
                crate::tr!("已连接电源")
            } else {
                crate::tr!("放电中")
            },
        );
        push(&category, &crate::tr!("型号"), battery.model.clone());
        push(
            &category,
            &crate::tr!("设计容量"),
            format!("{} mWh", battery.design_capacity_mwh),
        );
        push(
            &category,
            &crate::tr!("最大容量"),
            format!("{} mWh", battery.full_charge_capacity_mwh),
        );
    }

    rows
}

fn display_value(value: String) -> String {
    if value.trim().is_empty() {
        crate::tr!("未知")
    } else {
        value
    }
}

fn localized_architecture(value: &str) -> String {
    match value.trim() {
        "32 位" | "32-bit" => crate::tr!("32 位"),
        "64 位" | "64-bit" => crate::tr!("64 位"),
        _ => display_value(value.to_owned()),
    }
}

fn localized_network_status(value: &str) -> String {
    match value.trim() {
        // The fallback adapter collector translated this value when the startup snapshot was
        // created. Normalize both language variants so a later language switch is complete.
        "已连接" | "Connected" => crate::tr!("已连接"),
        _ => display_value(value.to_owned()),
    }
}

fn yes_no(value: bool) -> String {
    if value {
        crate::tr!("是")
    } else {
        crate::tr!("否")
    }
}

fn format_gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

fn bitlocker_status_text(status: &crate::core::hardware_info::BitLockerStatus) -> String {
    use crate::core::hardware_info::BitLockerStatus;
    match status {
        BitLockerStatus::Encrypted => crate::tr!("已加密"),
        BitLockerStatus::NotEncrypted => crate::tr!("未加密"),
        BitLockerStatus::EncryptionInProgress => crate::tr!("加密中"),
        BitLockerStatus::DecryptionInProgress => crate::tr!("解密中"),
        BitLockerStatus::Unknown => crate::tr!("未知"),
    }
}

pub struct AboutPage {
    pub product_name: HWND,
    pub version_label: HWND,
    pub version: HWND,
    pub description: HWND,
    pub language_label: HWND,
    pub language: HWND,
    pub refresh_languages: HWND,
    pub easy_mode: HWND,
    pub logging: HWND,
    pub wim_engine_label: HWND,
    pub wim_engine: HWND,
    pub download_threads_label: HWND,
    pub download_threads: HWND,
    pub advanced_options: HWND,
    pub experimental_mica: HWND,
    pub settings_help: HWND,
    pub credits: HWND,
    pub link_buttons: [HWND; 3],
    pub action_buttons: [HWND; 1],
    font: HFONT,
    languages: RefCell<Vec<crate::utils::i18n::LanguageInfo>>,
}

impl AboutPage {
    pub unsafe fn create(
        parent: HWND,
        font: HFONT,
        heading_font: HFONT,
        labels: &AboutLabels<'_>,
    ) -> windows::core::Result<Self> {
        let product_name = child(parent, w!("STATIC"), labels.product_name, 0, 5_212)?;
        let version_label = child(
            parent,
            w!("STATIC"),
            labels.version_label,
            SS_LEFTNOWORDWRAP_STYLE,
            5_213,
        )?;
        let version = child(parent, w!("STATIC"), labels.version, 0, ID_ABOUT_VERSION)?;
        let description = child(
            parent,
            w!("STATIC"),
            labels.description,
            0,
            ID_ABOUT_DESCRIPTION,
        )?;
        let language_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("界面语言:"),
            SS_LEFTNOWORDWRAP_STYLE,
            5_255,
        )?;
        let language = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_ABOUT_LANGUAGE,
        )?;
        let refresh_languages = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("刷新"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_ABOUT_REFRESH_LANGUAGES,
        )?;
        let easy_mode = child(
            parent,
            w!("BUTTON"),
            labels.easy_mode,
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_ABOUT_EASY_MODE,
        )?;
        let _ = SendMessageW(
            easy_mode,
            0x00F1,
            WPARAM(usize::from(labels.easy_mode_enabled)),
            LPARAM(0),
        );
        let _ = EnableWindow(easy_mode, labels.easy_mode_available);
        let logging = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("启用日志记录"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_ABOUT_LOGGING,
        )?;
        set_checked(logging, labels.log_enabled);
        let wim_engine_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("WIM 引擎:"),
            SS_LEFTNOWORDWRAP_STYLE,
            5_256,
        )?;
        let wim_engine = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_ABOUT_WIM_ENGINE,
        )?;
        add_combo_item(wim_engine, &crate::tr!("libwim（内置，默认）"));
        add_combo_item(wim_engine, &crate::tr!("wimgapi（系统原生 API）"));
        let _ = SendMessageW(
            wim_engine,
            CB_SETCURSEL,
            WPARAM(usize::from(labels.wim_engine == 1)),
            LPARAM(0),
        );
        let download_threads_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("下载线程:"),
            SS_LEFTNOWORDWRAP_STYLE,
            5_260,
        )?;
        let download_threads = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_ABOUT_DOWNLOAD_THREADS,
        )?;
        for threads in DOWNLOAD_THREAD_OPTIONS {
            add_combo_item(download_threads, &threads.to_string());
        }
        let normalized_threads =
            crate::core::app_config::normalize_download_threads(labels.download_threads);
        let selected_threads = DOWNLOAD_THREAD_OPTIONS
            .iter()
            .position(|threads| *threads == normalized_threads)
            .unwrap_or(1);
        let _ = SendMessageW(
            download_threads,
            CB_SETCURSEL,
            WPARAM(selected_threads),
            LPARAM(0),
        );
        let advanced_options = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("启用高级选项"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_ABOUT_ADVANCED,
        )?;
        set_checked(advanced_options, labels.advanced_options_enabled);
        let _ = EnableWindow(advanced_options, !labels.easy_mode_enabled);
        let experimental_mica = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("启用 Mica（实验性）"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_ABOUT_EXPERIMENTAL_MICA,
        )?;
        set_checked(experimental_mica, labels.experimental_mica_enabled);
        let settings_help = child(
            parent,
            w!("STATIC"),
            &crate::tr!("小白模式提供简化的系统重装界面；日志开关在下次启动时完全生效。\r\n下载线程数从下一个下载任务开始生效；镜像引擎同时用于正常系统端和 PE 端。"),
            0,
            5_257,
        )?;
        let credits = child(
            parent,
            w!("STATIC"),
            &crate::tr!("© 2026-present Cloud-PE Dev.  © 2026-present NORMAL-EX.\r\n部分系统镜像及 PE 下载服务由 Cloud-PE 云盘提供；感谢 电脑病毒爱好者 提供 WinPE。"),
            0,
            5_258,
        )?;

        let mut link_buttons = [HWND::default(); 3];
        for (index, link) in AboutLink::ALL.into_iter().enumerate() {
            link_buttons[index] = child(
                parent,
                w!("BUTTON"),
                labels.link_labels[index],
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                link.command_id(),
            )?;
        }
        let mut action_buttons = [HWND::default(); 1];
        for (index, action) in AboutAction::ALL.into_iter().enumerate() {
            action_buttons[index] = child(
                parent,
                w!("BUTTON"),
                &action.label(),
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                action.command_id(),
            )?;
        }
        let _ = EnableWindow(
            action_buttons[AboutAction::OpenLogDirectory as usize],
            labels.log_enabled,
        );

        let page = Self {
            product_name,
            version_label,
            version,
            description,
            language_label,
            language,
            refresh_languages,
            easy_mode,
            logging,
            wim_engine_label,
            wim_engine,
            download_threads_label,
            download_threads,
            advanced_options,
            experimental_mica,
            settings_help,
            credits,
            link_buttons,
            action_buttons,
            font,
            languages: RefCell::new(Vec::new()),
        };
        page.refresh_language_choices();
        page.apply_font(font, heading_font);
        page.show(false);
        Ok(page)
    }

    pub fn command_intent(command_id: u16) -> Option<InfoIntent> {
        match command_id {
            ID_ABOUT_LANGUAGE => return Some(InfoIntent::SelectLanguage),
            ID_ABOUT_REFRESH_LANGUAGES => return Some(InfoIntent::RefreshLanguages),
            ID_ABOUT_LOGGING => return Some(InfoIntent::ToggleLogging),
            ID_ABOUT_WIM_ENGINE => return Some(InfoIntent::SelectWimEngine),
            ID_ABOUT_DOWNLOAD_THREADS => return Some(InfoIntent::SelectDownloadThreads),
            ID_ABOUT_ADVANCED => return Some(InfoIntent::ToggleAdvancedOptions),
            ID_ABOUT_EXPERIMENTAL_MICA => return Some(InfoIntent::ToggleExperimentalMica),
            _ => {}
        }
        if command_id == ID_ABOUT_EASY_MODE {
            return Some(InfoIntent::ToggleEasyMode);
        }
        if let Some(index) = command_id.checked_sub(ID_FIRST_ABOUT_ACTION) {
            if let Some(action) = AboutAction::ALL.get(index as usize) {
                return Some(action.intent());
            }
        }
        let index = command_id.checked_sub(ID_FIRST_LINK)? as usize;
        AboutLink::ALL.get(index).copied().map(InfoIntent::OpenLink)
    }

    pub unsafe fn set_version(&self, version: &str) {
        set_text(self.version, version);
    }

    pub unsafe fn easy_mode_enabled(&self) -> bool {
        is_checked(self.easy_mode)
    }

    pub unsafe fn logging_enabled(&self) -> bool {
        is_checked(self.logging)
    }

    pub unsafe fn set_logging_enabled(&self, enabled: bool) {
        set_checked(self.logging, enabled);
        let _ = EnableWindow(
            self.action_buttons[AboutAction::OpenLogDirectory as usize],
            enabled,
        );
    }

    pub unsafe fn advanced_options_enabled(&self) -> bool {
        is_checked(self.advanced_options)
    }

    pub unsafe fn experimental_mica_enabled(&self) -> bool {
        is_checked(self.experimental_mica)
    }

    pub unsafe fn set_easy_mode_state(&self, enabled: bool, available: bool) {
        set_checked(self.easy_mode, enabled);
        let _ = EnableWindow(self.easy_mode, available);
        if enabled {
            set_checked(self.advanced_options, false);
        }
        let _ = EnableWindow(self.advanced_options, available && !enabled);
    }

    pub unsafe fn selected_wim_engine(&self) -> u8 {
        (SendMessageW(self.wim_engine, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 == 1) as u8
    }

    pub unsafe fn selected_download_threads(&self) -> u8 {
        let selected = SendMessageW(self.download_threads, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        usize::try_from(selected)
            .ok()
            .and_then(|index| DOWNLOAD_THREAD_OPTIONS.get(index))
            .copied()
            .unwrap_or(16)
    }

    pub unsafe fn selected_language_code(&self) -> Option<String> {
        let index = SendMessageW(self.language, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        (index >= 0)
            .then(|| {
                self.languages
                    .borrow()
                    .get(index as usize)
                    .map(|item| item.code.clone())
            })
            .flatten()
    }

    pub unsafe fn refresh_language_choices(&self) {
        crate::utils::i18n::refresh_available_languages();
        let languages = crate::utils::i18n::get_available_languages();
        let current = crate::utils::i18n::current_language();
        let _ = SendMessageW(self.language, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        let mut selected = 0usize;
        for (index, item) in languages.iter().enumerate() {
            add_combo_item(self.language, &item.display_name);
            if item.code == current {
                selected = index;
            }
        }
        let _ = SendMessageW(self.language, CB_SETCURSEL, WPARAM(selected), LPARAM(0));
        *self.languages.borrow_mut() = languages;
    }

    pub unsafe fn relocalize(&self, easy_mode_available: bool) {
        set_text(self.version_label, &crate::tr!("版本:"));
        set_text(
            self.description,
            &crate::tr!("Windows 系统安装、备份和维护工具。"),
        );
        set_text(self.language_label, &crate::tr!("界面语言:"));
        set_text(self.refresh_languages, &crate::tr!("刷新"));
        set_text(self.easy_mode, &crate::tr!("启用小白模式"));
        set_text(self.logging, &crate::tr!("启用日志记录"));
        set_text(self.wim_engine_label, &crate::tr!("WIM 引擎:"));
        set_text(self.download_threads_label, &crate::tr!("下载线程:"));
        set_text(self.advanced_options, &crate::tr!("启用高级选项"));
        set_text(self.experimental_mica, &crate::tr!("启用 Mica（实验性）"));
        set_text(
            self.settings_help,
            &crate::tr!("小白模式提供简化的系统重装界面；日志开关在下次启动时完全生效。\r\n下载线程数从下一个下载任务开始生效；镜像引擎同时用于正常系统端和 PE 端。"),
        );
        set_text(
            self.credits,
            &crate::tr!("© 2026-present Cloud-PE Dev.  © 2026-present NORMAL-EX.\r\n部分系统镜像及 PE 下载服务由 Cloud-PE 云盘提供；感谢 电脑病毒爱好者 提供 WinPE。"),
        );
        let link_labels = [
            crate::tr!("项目主页"),
            crate::tr!("问题反馈"),
            crate::tr!("开源许可"),
        ];
        for (control, label) in self.link_buttons.into_iter().zip(link_labels) {
            set_text(control, &label);
        }
        for (control, action) in self.action_buttons.into_iter().zip(AboutAction::ALL) {
            set_text(control, &action.label());
        }
        let selected_engine = self.selected_wim_engine();
        let _ = SendMessageW(self.wim_engine, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        add_combo_item(self.wim_engine, &crate::tr!("libwim（内置，默认）"));
        add_combo_item(self.wim_engine, &crate::tr!("wimgapi（系统原生 API）"));
        let _ = SendMessageW(
            self.wim_engine,
            CB_SETCURSEL,
            WPARAM(selected_engine as usize),
            LPARAM(0),
        );
        self.set_easy_mode_state(self.easy_mode_enabled(), easy_mode_available);
        self.refresh_language_choices();
    }

    pub unsafe fn layout(&self, rect: PageRect, dpi: u32) {
        let s = |value: i32| value * dpi as i32 / 96;
        let width = rect.width.max(0);
        let gap = s(8);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let field_height = metrics.field_height;
        let row_height = s(30);
        let button_layout = about_button_layout(width, dpi);

        let _ = MoveWindow(
            self.product_name,
            rect.x,
            rect.y,
            width,
            s(28).min(rect.height.max(0)),
            true,
        );
        let version_y = rect.y + s(36);
        let version_label_width = s(72).min(width / 3);
        let _ = MoveWindow(
            self.version_label,
            rect.x,
            version_y + s(4),
            version_label_width,
            s(22),
            true,
        );
        let _ = MoveWindow(
            self.version,
            rect.x + version_label_width,
            version_y,
            (width - version_label_width).max(0),
            s(28),
            true,
        );

        let description_y = version_y + s(38);
        // The description is a single short sentence. Giving it all spare vertical space
        // pushes every setting and the credits into the status bar on ordinary window sizes.
        // Keep the page as a compact top-down flow; the remaining controls still wrap by width.
        let description_height = s(28);
        let _ = MoveWindow(
            self.description,
            rect.x,
            description_y,
            width,
            description_height,
            true,
        );

        let settings_x = rect.x;
        let language_y = description_y + description_height + gap;
        let label_width = settings_label_width(
            &[
                measure_text(
                    self.language_label,
                    self.font,
                    &crate::tr!("界面语言:"),
                    None,
                )
                .width,
                measure_text(
                    self.wim_engine_label,
                    self.font,
                    &crate::tr!("WIM 引擎:"),
                    None,
                )
                .width,
                measure_text(
                    self.download_threads_label,
                    self.font,
                    &crate::tr!("下载线程:"),
                    None,
                )
                .width,
            ],
            width,
            dpi,
        );
        let refresh_width = s(76).min(width / 4);
        // Keep the selector close to its actual longest item instead of stretching it
        // across the page. The remaining space is intentionally left after Refresh.
        let language_width = (width - label_width - refresh_width - gap)
            .min(s(280))
            .max(0);
        let language_closed_height = combo_closed_height(self.language, field_height);
        let language_row_height = language_closed_height.max(field_height);
        let _ = MoveWindow(
            self.language_label,
            settings_x,
            centered_control_y_ceil(language_y, language_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.language,
            settings_x + label_width,
            centered_control_y_ceil(language_y, language_row_height, language_closed_height),
            language_width,
            s(220),
            true,
        );
        let _ = MoveWindow(
            self.refresh_languages,
            settings_x + label_width + language_width + gap,
            centered_control_y_ceil(language_y, language_row_height, language_closed_height),
            refresh_width,
            language_closed_height,
            true,
        );
        let easy_y = language_y + language_row_height + gap;
        let half = (width - gap) / 2;
        let _ = MoveWindow(self.easy_mode, settings_x, easy_y, half, s(26), true);
        let _ = MoveWindow(
            self.logging,
            settings_x + half + gap,
            easy_y,
            half,
            s(26),
            true,
        );
        let engine_y = easy_y + s(26) + gap;
        let engine_width = (width - label_width).min(s(280)).max(0);
        let engine_closed_height = combo_closed_height(self.wim_engine, field_height);
        let engine_row_height = engine_closed_height.max(field_height);
        let _ = MoveWindow(
            self.wim_engine_label,
            settings_x,
            centered_control_y_ceil(engine_y, engine_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.wim_engine,
            settings_x + label_width,
            centered_control_y_ceil(engine_y, engine_row_height, engine_closed_height),
            engine_width,
            s(220),
            true,
        );
        let download_threads_y = engine_y + engine_row_height + gap;
        let download_threads_width = s(88).min((width - label_width).max(0));
        let threads_closed_height = combo_closed_height(self.download_threads, field_height);
        let threads_row_height = threads_closed_height.max(field_height);
        let _ = MoveWindow(
            self.download_threads_label,
            settings_x,
            centered_control_y_ceil(download_threads_y, threads_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            self.download_threads,
            settings_x + label_width,
            centered_control_y_ceil(
                download_threads_y,
                threads_row_height,
                threads_closed_height,
            ),
            download_threads_width,
            s(300),
            true,
        );
        let advanced_y = download_threads_y + threads_row_height + gap;
        let settings_half = (width - gap) / 2;
        let _ = MoveWindow(
            self.advanced_options,
            settings_x,
            advanced_y,
            settings_half,
            s(26),
            true,
        );
        let _ = MoveWindow(
            self.experimental_mica,
            settings_x + settings_half + gap,
            advanced_y,
            (width - settings_half - gap).max(0),
            s(26),
            true,
        );
        let help_y = advanced_y + s(26) + gap;
        let help_height = s(48);
        let _ = MoveWindow(
            self.settings_help,
            settings_x,
            help_y,
            width,
            help_height,
            true,
        );
        let credits_y = help_y + help_height + gap;
        let credits_height = s(44);
        let _ = MoveWindow(
            self.credits,
            settings_x,
            credits_y,
            width,
            credits_height,
            true,
        );
        let buttons_y = credits_y + credits_height + gap;
        for index in 0..3 {
            let column = index as i32 % button_layout.columns;
            let row = index as i32 / button_layout.columns;
            let _ = MoveWindow(
                self.link_buttons[index],
                rect.x + column * (button_layout.button_width + gap),
                buttons_y + row * (row_height + gap),
                button_layout.button_width,
                row_height,
                true,
            );
        }

        for (index, button) in self.action_buttons.iter().copied().enumerate() {
            let unified_index = 3 + index as i32;
            let column = unified_index % button_layout.columns;
            let row = unified_index / button_layout.columns;
            let _ = MoveWindow(
                button,
                rect.x + column * (button_layout.button_width + gap),
                buttons_y + row * (row_height + gap),
                button_layout.button_width,
                row_height,
                true,
            );
        }
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for hwnd in self.controls() {
            let _ = ShowWindow(hwnd, command);
        }
    }

    pub unsafe fn apply_font(&self, font: HFONT, heading_font: HFONT) {
        for hwnd in self.controls() {
            let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        let _ = SendMessageW(
            self.product_name,
            WM_SETFONT,
            WPARAM(heading_font.0 as usize),
            LPARAM(1),
        );
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        for control in [
            self.easy_mode,
            self.logging,
            self.advanced_options,
            self.experimental_mica,
            self.refresh_languages,
        ] {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        for control in [self.language, self.wim_engine, self.download_threads] {
            apply_control_theme(control, palette, NativeControlKind::Field);
        }
        for control in self
            .link_buttons
            .iter()
            .chain(self.action_buttons.iter())
            .copied()
        {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
    }

    fn controls(&self) -> impl Iterator<Item = HWND> + '_ {
        [
            self.product_name,
            self.version_label,
            self.version,
            self.description,
            self.language_label,
            self.language,
            self.refresh_languages,
            self.easy_mode,
            self.logging,
            self.wim_engine_label,
            self.wim_engine,
            self.download_threads_label,
            self.download_threads,
            self.advanced_options,
            self.experimental_mica,
            self.settings_help,
            self.credits,
        ]
        .into_iter()
        .chain(self.link_buttons)
        .chain(self.action_buttons)
    }
}

unsafe fn set_checked(control: HWND, checked: bool) {
    let _ = SendMessageW(control, 0x00F1, WPARAM(usize::from(checked)), LPARAM(0));
}

unsafe fn is_checked(control: HWND) -> bool {
    SendMessageW(control, 0x00F0, WPARAM(0), LPARAM(0)).0 == 1
}

unsafe fn add_combo_item(combo: HWND, text: &str) {
    let text = wide(text);
    let _ = SendMessageW(
        combo,
        CB_ADDSTRING,
        WPARAM(0),
        LPARAM(text.as_ptr() as isize),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AboutButtonLayout {
    columns: i32,
    button_width: i32,
}

fn about_button_layout(width: i32, dpi: u32) -> AboutButtonLayout {
    let s = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
    let gap = s(8);
    let minimum = s(140);
    let columns = ((width + gap) / (minimum + gap)).clamp(1, 4);
    AboutButtonLayout {
        columns,
        button_width: ((width - gap * (columns - 1)) / columns).max(0),
    }
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_commands_are_stable_and_side_effect_free() {
        assert_eq!(
            HardwareInfoPage::command_intent(ID_HARDWARE_SAVE),
            Some(InfoIntent::SaveHardwareText)
        );
        for link in AboutLink::ALL {
            assert_eq!(
                AboutPage::command_intent(link.command_id()),
                Some(InfoIntent::OpenLink(link))
            );
        }
        assert_eq!(AboutPage::command_intent(ID_FIRST_LINK + 3), None);
        for action in AboutAction::ALL {
            assert_eq!(
                AboutPage::command_intent(action.command_id()),
                Some(action.intent())
            );
        }
        for (command, intent) in [
            (ID_ABOUT_LANGUAGE, InfoIntent::SelectLanguage),
            (ID_ABOUT_REFRESH_LANGUAGES, InfoIntent::RefreshLanguages),
            (ID_ABOUT_EASY_MODE, InfoIntent::ToggleEasyMode),
            (ID_ABOUT_LOGGING, InfoIntent::ToggleLogging),
            (ID_ABOUT_WIM_ENGINE, InfoIntent::SelectWimEngine),
            (ID_ABOUT_DOWNLOAD_THREADS, InfoIntent::SelectDownloadThreads),
            (ID_ABOUT_ADVANCED, InfoIntent::ToggleAdvancedOptions),
            (
                ID_ABOUT_EXPERIMENTAL_MICA,
                InfoIntent::ToggleExperimentalMica,
            ),
        ] {
            assert_eq!(AboutPage::command_intent(command), Some(intent));
        }
    }

    #[test]
    fn about_buttons_share_one_compact_responsive_grid() {
        let compact = about_button_layout(300, 96);
        assert_eq!(compact.columns, 2);
        assert_eq!(compact.button_width, 146);
        let high_dpi = about_button_layout(600, 192);
        assert_eq!(high_dpi.columns, 2);
        assert_eq!(high_dpi.button_width, 292);
        let narrow = about_button_layout(180, 192);
        assert_eq!(narrow.columns, 1);
        assert_eq!(narrow.button_width, 180);
    }

    #[test]
    fn about_label_column_follows_the_longest_translation_without_overgrowing() {
        assert_eq!(settings_label_width(&[72, 95, 142], 900, 96), 150);
        assert_eq!(settings_label_width(&[72, 95, 142], 300, 96), 100);
        assert_eq!(settings_label_width(&[144, 190, 284], 1_800, 192), 300);
    }

    #[test]
    fn hardware_columns_preserve_a_readable_value_column_on_narrow_windows() {
        let narrow = hardware_column_widths(420, 96);
        assert!(narrow[0] >= 88);
        assert!(narrow[1] >= 128);
        assert!(narrow[2] >= 300);
        assert!(narrow.iter().sum::<i32>() > 420);

        let high_dpi = hardware_column_widths(1_600, 192);
        assert!(high_dpi[0] >= 176);
        assert!(high_dpi[1] >= 256);
        assert!(high_dpi[2] >= 600);

        let ordinary = hardware_column_widths(926, 96);
        assert!(ordinary[0] <= 120);
        assert!(ordinary[0] < ordinary[1]);
    }

    #[test]
    fn hardware_structure_maps_to_category_item_value_rows() {
        let info = crate::core::hardware_info::HardwareInfo {
            computer_name: "TEST-PC".into(),
            cpu: crate::core::hardware_info::CpuInfo {
                name: "Example CPU".into(),
                ..Default::default()
            },
            memory: crate::core::hardware_info::MemoryInfo {
                total_physical: 16 * 1024 * 1024 * 1024,
                ..Default::default()
            },
            gpus: vec![crate::core::hardware_info::GpuInfo {
                name: "Example GPU".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rows = hardware_info_rows(&info, None);
        assert!(rows.iter().any(|row| row.value == "TEST-PC"));
        assert!(rows.iter().any(|row| row.value == "Example CPU"));
        assert!(rows.iter().any(|row| row.value.contains("16.0 GiB")));
        assert!(rows.iter().any(|row| row.value.contains("Example GPU")));
        assert!(rows
            .iter()
            .all(|row| !row.item.is_empty() && !row.value.is_empty()));
        assert!(rows.iter().any(|row| row.category.is_empty()));
        assert!(rows
            .windows(2)
            .all(|pair| pair[0].category.is_empty() || pair[0].category != pair[1].category));
    }

    #[test]
    fn hardware_export_groups_categories_and_keeps_one_item_per_line() {
        let rows = vec![
            HardwareInfoRow::new("System", "Operating system", "Windows 11 Pro"),
            HardwareInfoRow::new("", "Version", "25H2"),
            // Accept fully populated category cells as well as the blank repeated cells used by
            // the ListView. The export still emits one heading for the contiguous group.
            HardwareInfoRow::new("System", "Build", "26200.8737"),
            HardwareInfoRow::new("Graphics", "GPU 1", "GeForce RTX 4060"),
            HardwareInfoRow::new("", "Driver", "591.86\r\n2026-07-01"),
        ];

        let report = format_hardware_report(&rows);
        assert_eq!(
            report,
            "[System]\r\nOperating system: Windows 11 Pro\r\nVersion: 25H2\r\nBuild: 26200.8737\r\n\r\n[Graphics]\r\nGPU 1: GeForce RTX 4060\r\nDriver: 591.86\r\n    2026-07-01"
        );
        assert_eq!(report.matches("[System]").count(), 1);
        assert_eq!(report.matches("Operating system:").count(), 1);
        assert!(!report.contains('\t'));
    }

    #[test]
    fn hardware_export_is_equally_stable_for_chinese_labels() {
        let rows = vec![
            HardwareInfoRow::new("系统", "操作系统", "Windows 11 专业版"),
            HardwareInfoRow::new("", "计算机名", "测试电脑"),
            HardwareInfoRow::new("内存", "内存条 1", "镁光 | 8.0 GiB | 4800 MHz"),
        ];

        assert_eq!(
            format_hardware_report(&rows),
            "[系统]\r\n操作系统: Windows 11 专业版\r\n计算机名: 测试电脑\r\n\r\n[内存]\r\n内存条 1: 镁光 | 8.0 GiB | 4800 MHz"
        );
    }
}
