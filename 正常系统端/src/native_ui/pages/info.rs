//! Native hardware-information and about pages.
//!
//! Hardware values are presented as a responsive category/item/value report ListView;
//! the page also retains a complete tab-separated representation for copying. The about page
//! exposes the existing persisted language, logging, easy-mode, download-connection,
//! image-engine preferences. Link and settings controls emit intents; this
//! module never starts a browser or opens filesystem locations.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, HFONT, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindowVisible, SendMessageW, SetWindowTextW, ShowWindow, BS_AUTOCHECKBOX, BS_OWNERDRAW,
    CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, SW_HIDE, SW_SHOW,
    WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use super::download::PageRect;
use crate::native_ui::controls::{child, move_layout_window as MoveWindow, wide};
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
const ID_ABOUT_AUTOMATION_EXPORT: u16 = 5_254;
const ID_ABOUT_AUTOMATIC_FEEDBACK: u16 = 5_261;
const ID_ABOUT_DOWNLOAD_THREADS: u16 = 5_259;
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
    ToggleAutomationExport,
    ToggleAutomaticFeedback,
    SelectWimEngine,
    SelectDownloadThreads,
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
    pub mode: crate::core::app_config::UiMode,
    pub log_enabled: bool,
    pub automation_export_enabled: bool,
    pub automatic_feedback_enabled: bool,
    pub wim_engine: u8,
    pub download_threads: u8,
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
        #[cfg(feature = "non-elevated-tests")]
        let rows = if std::env::var_os("LETRECOVERY_UI_QA_LONG_LIST").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            qa_long_hardware_rows()
        } else {
            rows
        };
        // Hardware enumeration can complete while this page is already visible. All mutations run
        // inside the current window message and are published with one synchronous final redraw.
        // Do not toggle WM_SETREDRAW on this child: DefWindowProc implements it by changing
        // WS_VISIBLE, which can make DWM publish a redirected frame without the real Header child.
        let visible = IsWindowVisible(self.report).as_bool();
        let _ = SendMessageW(self.report, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        for (index, row) in rows.iter().enumerate() {
            insert_hardware_item(self.report, index as i32, 0, &row.category);
            insert_hardware_item(self.report, index as i32, 1, &row.item);
            insert_hardware_item(self.report, index as i32, 2, &row.value);
        }
        *self.rows.borrow_mut() = rows;
        if visible {
            let _ = RedrawWindow(
                self.report,
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
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
            false,
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
            false,
        );
        let widths = hardware_column_widths(rect.width, dpi);
        // After the report has its new size, inside the same layout commit: the report then
        // recalculates once per column change with its final size, and its scrollbar redraws
        // are folded into the step's single paint pass.
        let report = self.report;
        crate::native_ui::controls::after_layout_commit(move || {
            for (index, width) in widths.into_iter().enumerate() {
                let _ = SendMessageW(
                    report,
                    LVM_SETCOLUMNWIDTH,
                    WPARAM(index),
                    LPARAM(width as isize),
                );
            }
        });
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

#[cfg(feature = "non-elevated-tests")]
fn qa_long_hardware_rows() -> Vec<HardwareInfoRow> {
    (1..=120)
        .map(|index| {
            HardwareInfoRow::new(
                format!("QA category {:02}", (index - 1) / 10 + 1),
                format!("Scrollable hardware row {index:03}"),
                format!("Deterministic preview value {index:03}"),
            )
        })
        .collect()
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
            network_link_status_text(&localized_network_status(&adapter.status), adapter.speed),
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

fn network_link_status_text(status: &str, speed_bps: u64) -> String {
    if speed_bps == 0 {
        format!("{} | {}", status, crate::tr!("未知"))
    } else {
        format!("{} | {} Mbps", status, speed_bps / 1_000_000)
    }
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
    const MIB: f64 = 1024.0 * 1024.0;
    if (bytes as f64) < 1024.0 * MIB {
        format!("{:.0} MiB", bytes as f64 / MIB)
    } else {
        format!("{:.1} GiB", bytes as f64 / 1024.0 / MIB)
    }
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
    pub mode_label: HWND,
    pub logging: HWND,
    pub automation_export: HWND,
    pub automatic_feedback: HWND,
    pub wim_engine_label: HWND,
    pub wim_engine: HWND,
    pub download_threads_label: HWND,
    pub download_threads: HWND,
    pub settings_help: HWND,
    pub credits: HWND,
    pub link_buttons: [HWND; 3],
    pub action_buttons: [HWND; 1],
    advanced: Cell<bool>,
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
        // 语言列表在启动和切换语言时自动刷新，不再单独提供“刷新”按钮。
        let refresh_languages = HWND::default();
        let _ = (labels.easy_mode, labels.easy_mode_enabled);
        let mode_label = child(
            parent,
            w!("STATIC"),
            &crate::tr!("使用模式:"),
            SS_LEFTNOWORDWRAP_STYLE,
            5_262,
        )?;
        // 原来的“启用小白模式”勾选框改为三档使用模式：小白 / 正常 / 高级。
        let easy_mode = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_ABOUT_EASY_MODE,
        )?;
        fill_mode_combo(easy_mode, labels.mode);
        let _ = EnableWindow(easy_mode, labels.easy_mode_available);
        // 日志始终开启（排查问题必需），不再提供开关。
        let logging = HWND::default();
        let automation_export = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("显示“导出自动化配置”按钮"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_ABOUT_AUTOMATION_EXPORT,
        )?;
        set_checked(automation_export, labels.automation_export_enabled);
        let automatic_feedback = child(
            parent,
            w!("BUTTON"),
            &crate::tr!("发送匿名统计"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_ABOUT_AUTOMATIC_FEEDBACK,
        )?;
        set_checked(automatic_feedback, labels.automatic_feedback_enabled);
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
        let settings_help = child(
            parent,
            w!("STATIC"),
            &crate::tr!("错误报告只包含出错原因和电脑环境；安装统计只有版本号和安装方式。都不包含你的文件、账户和密码。"),
            0,
            5_257,
        )?;
        let credits = child(
            parent,
            w!("STATIC"),
            &crate::build_info::credits(),
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

        for (control, (name, _)) in link_buttons
            .into_iter()
            .zip(lr_core::branding::promotions())
        {
            set_text(control, &name);
        }

        let page = Self {
            product_name,
            version_label,
            version,
            description,
            language_label,
            language,
            refresh_languages,
            easy_mode,
            mode_label,
            logging,
            automation_export,
            automatic_feedback,
            wim_engine_label,
            wim_engine,
            download_threads_label,
            download_threads,
            settings_help,
            credits,
            link_buttons,
            action_buttons,
            advanced: Cell::new(labels.mode == crate::core::app_config::UiMode::Advanced),
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
            ID_ABOUT_AUTOMATION_EXPORT => return Some(InfoIntent::ToggleAutomationExport),
            ID_ABOUT_AUTOMATIC_FEEDBACK => return Some(InfoIntent::ToggleAutomaticFeedback),
            ID_ABOUT_WIM_ENGINE => return Some(InfoIntent::SelectWimEngine),
            ID_ABOUT_DOWNLOAD_THREADS => return Some(InfoIntent::SelectDownloadThreads),
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

    pub unsafe fn selected_mode(&self) -> crate::core::app_config::UiMode {
        mode_from_index(SendMessageW(self.easy_mode, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0)
    }

    pub unsafe fn easy_mode_enabled(&self) -> bool {
        self.selected_mode() == crate::core::app_config::UiMode::Beginner
    }

    pub unsafe fn logging_enabled(&self) -> bool {
        is_checked(self.logging)
    }

    pub unsafe fn automation_export_enabled(&self) -> bool {
        is_checked(self.automation_export)
    }

    pub unsafe fn set_automation_export_enabled(&self, enabled: bool) {
        set_checked(self.automation_export, enabled);
    }

    pub unsafe fn automatic_feedback_enabled(&self) -> bool {
        is_checked(self.automatic_feedback)
    }
    pub unsafe fn set_automatic_feedback_enabled(&self, enabled: bool) {
        set_checked(self.automatic_feedback, enabled);
    }


    pub unsafe fn set_logging_enabled(&self, enabled: bool) {
        set_checked(self.logging, enabled);
        let _ = EnableWindow(
            self.action_buttons[AboutAction::OpenLogDirectory as usize],
            enabled,
        );
    }

    /// 同步使用模式下拉框。高级设置（WIM 引擎、下载线程、自动化导出）只在高级模式下显示；
    /// 调用方随后重新布局并刷新页面可见性。
    pub unsafe fn set_mode_state(&self, mode: crate::core::app_config::UiMode, available: bool) {
        let _ = SendMessageW(
            self.easy_mode,
            CB_SETCURSEL,
            WPARAM(mode_index(mode)),
            LPARAM(0),
        );
        let _ = EnableWindow(self.easy_mode, available);
        self.advanced
            .set(mode == crate::core::app_config::UiMode::Advanced);
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
        set_text(self.product_name, &crate::build_info::product_name());
        set_text(self.version_label, &crate::tr!("版本:"));
        set_text(self.version, &crate::build_info::display_version());
        set_text(self.description, &crate::build_info::description());
        set_text(self.language_label, &crate::tr!("界面语言:"));
        set_text(self.refresh_languages, &crate::tr!("刷新"));
        set_text(self.mode_label, &crate::tr!("使用模式:"));
        set_text(self.logging, &crate::tr!("启用日志记录"));
        set_text(
            self.automation_export,
            &crate::tr!("显示“导出自动化配置”按钮"),
        );
        set_text(self.automatic_feedback, &crate::tr!("发送匿名统计"));
        set_text(self.wim_engine_label, &crate::tr!("WIM 引擎:"));
        set_text(self.download_threads_label, &crate::tr!("下载线程:"));
        set_text(
            self.settings_help,
            &crate::tr!("错误报告只包含出错原因和电脑环境；安装统计只有版本号和安装方式。都不包含你的文件、账户和密码。"),
        );
        set_text(self.credits, &crate::build_info::credits());
        let link_labels = [
            crate::tr!("项目主页"),
            crate::tr!("问题反馈"),
            crate::tr!("许可协议"),
        ];
        for (control, label) in self.link_buttons.into_iter().zip(link_labels) {
            set_text(control, &label);
        }
        for (control, (name, _)) in self
            .link_buttons
            .into_iter()
            .zip(lr_core::branding::promotions())
        {
            set_text(control, &name);
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
        let mode = self.selected_mode();
        fill_mode_combo(self.easy_mode, mode);
        self.set_mode_state(mode, easy_mode_available);
        self.refresh_language_choices();
    }

    pub unsafe fn layout(&self, rect: PageRect, dpi: u32) {
        let s = |value: i32| value * dpi as i32 / 96;
        let width = rect.width.max(0);
        let gap = s(8);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let field_height = metrics.field_height;
        let settings_x = rect.x;
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
        // 界面语言和使用模式上下两行排列，共用同一列标签宽度，避免右侧大片留白。
        let label_width = label_width.max(
            measure_text(self.mode_label, self.font, &crate::tr!("使用模式:"), None).width + s(8),
        );
        // Every drop-down list is exactly as wide as its own longest item in the current font
        // and DPI (plus the closed-field text inset and the arrow), so short choices such as
        // "简体中文" no longer sit in a mostly empty field.
        let field_space = (width - label_width).max(0);
        let language_width = fitted_combo_width(self.language, self.font, dpi, field_space);
        let mode_label_width = label_width;
        let mode_x = settings_x;
        let mode_width = fitted_combo_width(self.easy_mode, self.font, dpi, field_space);
        let language_closed_height = combo_closed_height(self.language, field_height);
        let language_row_height = language_closed_height.max(field_height);
        let half = (width - gap) / 2;
        // Two check boxes per row when every caption fits half the width; otherwise one per row.
        let advanced = self.advanced.get();
        let mut check_boxes: Vec<HWND> = Vec::new();
        if lr_core::branding::feature::AUTOMATIC_FEEDBACK
            || lr_core::branding::feature::INSTALL_STATS
        {
            check_boxes.push(self.automatic_feedback);
        }
        if advanced && lr_core::branding::feature::AUTOMATION_EXPORT {
            check_boxes.push(self.automation_export);
        }
        let widest = check_boxes
            .iter()
            .map(|check| crate::native_ui::layout::control_text_width(*check) + s(24))
            .max()
            .unwrap_or(0);
        let per_row = if widest <= half { 2 } else { 1 };
        let check_width = if per_row == 2 { half } else { width };
        let check_rows = (check_boxes.len() as i32 + per_row - 1) / per_row;
        let engine_width = fitted_combo_width(self.wim_engine, self.font, dpi, field_space);
        let engine_closed_height = combo_closed_height(self.wim_engine, field_height);
        let engine_row_height = if advanced {
            engine_closed_height.max(field_height)
        } else {
            0
        };
        let download_threads_width =
            fitted_combo_width(self.download_threads, self.font, dpi, field_space);
        let threads_closed_height = combo_closed_height(self.download_threads, field_height);
        let threads_row_height = if advanced {
            threads_closed_height.max(field_height)
        } else {
            0
        };
        // 关于页底部不再放项目主页等按钮；商业定制版在这里显示推广软件（最多 3 个）。
        let buttons: Vec<HWND> = self
            .link_buttons
            .iter()
            .copied()
            .take(lr_core::branding::promotions().len())
            .collect();
        let caption_widths: Vec<i32> = buttons
            .iter()
            .map(|button| crate::native_ui::layout::control_text_width(*button))
            .collect();
        let flow = about_flow_layout(&AboutFlowInput {
            available_height: rect.height.max(0),
            width,
            dpi,
            language_row_height: language_row_height * 2 + gap,
            check_rows,
            engine_row_height,
            threads_row_height,
            // 说明文字已移除，不再占位。
            help_text_height: 0,
            credits_text_height: crate::native_ui::layout::control_wrapped_height(
                self.credits,
                width,
            ),
            button_caption_widths: &caption_widths,
        });

        let _ = MoveWindow(
            self.product_name,
            rect.x,
            rect.y,
            width,
            s(28).min(rect.height.max(0)),
            false,
        );
        let version_y = rect.y + flow.version_y;
        let version_label_width = s(72).min(width / 3);
        let _ = MoveWindow(
            self.version_label,
            rect.x,
            version_y + s(4),
            version_label_width,
            s(22),
            false,
        );
        let _ = MoveWindow(
            self.version,
            rect.x + version_label_width,
            version_y,
            (width - version_label_width).max(0),
            s(28),
            false,
        );
        // The description is a single short sentence; only the last compaction step hides it.
        let _ = MoveWindow(
            self.description,
            rect.x,
            rect.y + flow.description_y,
            width,
            flow.description_height,
            false,
        );

        let language_y = rect.y + flow.language_y;
        let _ = MoveWindow(
            self.language_label,
            settings_x,
            centered_control_y_ceil(language_y, language_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            false,
        );
        let _ = MoveWindow(
            self.language,
            settings_x + label_width,
            centered_control_y_ceil(language_y, language_row_height, language_closed_height),
            language_width,
            s(220),
            false,
        );
        let mode_y = language_y + language_row_height + gap;
        let _ = MoveWindow(
            self.mode_label,
            mode_x,
            centered_control_y_ceil(mode_y, language_row_height, metrics.label_height),
            mode_label_width,
            metrics.label_height,
            false,
        );
        let _ = MoveWindow(
            self.easy_mode,
            mode_x + mode_label_width,
            centered_control_y_ceil(mode_y, language_row_height, language_closed_height),
            mode_width,
            s(220),
            false,
        );
        for (index, check) in check_boxes.into_iter().enumerate() {
            let column = index as i32 % per_row;
            let row = index as i32 / per_row;
            let _ = MoveWindow(
                check,
                settings_x + column * (half + gap),
                rect.y + flow.check_y + row * flow.check_step,
                check_width,
                s(26),
                false,
            );
        }
        let engine_y = rect.y + flow.engine_y;
        let _ = MoveWindow(
            self.wim_engine_label,
            settings_x,
            centered_control_y_ceil(engine_y, engine_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            false,
        );
        let _ = MoveWindow(
            self.wim_engine,
            settings_x + label_width,
            centered_control_y_ceil(engine_y, engine_row_height, engine_closed_height),
            engine_width,
            s(220),
            false,
        );
        let download_threads_y = rect.y + flow.threads_y;
        let _ = MoveWindow(
            self.download_threads_label,
            settings_x,
            centered_control_y_ceil(download_threads_y, threads_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            false,
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
            false,
        );
        let _ = MoveWindow(
            self.settings_help,
            settings_x,
            rect.y + flow.help_y,
            width,
            flow.help_height,
            false,
        );
        let _ = MoveWindow(
            self.credits,
            settings_x,
            rect.y + flow.credits_y,
            width,
            flow.credits_height,
            false,
        );
        for (button, placement) in buttons.iter().zip(&flow.buttons) {
            let _ = MoveWindow(
                *button,
                rect.x + placement.x,
                rect.y + placement.y,
                placement.width,
                flow.button_height,
                false,
            );
        }
    }

    pub unsafe fn show(&self, visible: bool) {
        let advanced = self.advanced.get();
        let advanced_only = [
            self.automation_export,
            self.wim_engine_label,
            self.wim_engine,
            self.download_threads_label,
            self.download_threads,
        ];
        for hwnd in self.controls() {
            let shown = visible
                && hwnd != self.settings_help
                && (advanced || !advanced_only.contains(&hwnd))
                && (hwnd != self.automation_export
                    || lr_core::branding::feature::AUTOMATION_EXPORT)
                && (hwnd != self.automatic_feedback
                    || lr_core::branding::feature::AUTOMATIC_FEEDBACK
                    || lr_core::branding::feature::INSTALL_STATS);
            let _ = ShowWindow(hwnd, if shown { SW_SHOW } else { SW_HIDE });
        }
        // 说明文字和底部按钮已移除：控件仍保留以兼容原有消息处理，但始终隐藏。
        let _ = ShowWindow(self.settings_help, SW_HIDE);
        let promoted = lr_core::branding::promotions().len();
        for (index, hwnd) in self.link_buttons.iter().enumerate() {
            let _ = ShowWindow(
                *hwnd,
                if visible && index < promoted {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        for hwnd in self.action_buttons.iter() {
            let _ = ShowWindow(*hwnd, SW_HIDE);
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
        for control in [self.automation_export, self.automatic_feedback] {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        for control in [
            self.language,
            self.easy_mode,
            self.wim_engine,
            self.download_threads,
        ] {
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
            self.mode_label,
            self.easy_mode,
            self.automation_export,
            self.automatic_feedback,
            self.wim_engine_label,
            self.wim_engine,
            self.download_threads_label,
            self.download_threads,
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

fn mode_index(mode: crate::core::app_config::UiMode) -> usize {
    use crate::core::app_config::UiMode;
    let offset = usize::from(lr_core::branding::feature::EASY_MODE);
    match mode {
        UiMode::Beginner => 0,
        UiMode::Normal => offset,
        UiMode::Advanced => offset + 1,
    }
}

fn mode_from_index(index: isize) -> crate::core::app_config::UiMode {
    use crate::core::app_config::UiMode;
    let offset = isize::from(lr_core::branding::feature::EASY_MODE);
    match index - offset {
        -1 => UiMode::Beginner,
        1 => UiMode::Advanced,
        _ => UiMode::Normal,
    }
}

unsafe fn fill_mode_combo(combo: HWND, mode: crate::core::app_config::UiMode) {
    let _ = SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    if lr_core::branding::feature::EASY_MODE {
        add_combo_item(combo, &crate::tr!("小白模式（只保留一键重装）"));
    }
    add_combo_item(combo, &crate::tr!("正常模式（推荐）"));
    add_combo_item(combo, &crate::tr!("高级模式（显示全部选项）"));
    let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(mode_index(mode)), LPARAM(0));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AboutButtonLayout {
    columns: i32,
    button_width: i32,
}

fn about_button_layout(width: i32, dpi: u32, widest_caption: i32) -> AboutButtonLayout {
    let s = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
    let gap = s(8);
    let minimum = s(140).max(widest_caption + s(24));
    let columns = ((width + gap) / (minimum + gap)).clamp(1, 4);
    AboutButtonLayout {
        columns,
        button_width: ((width - gap * (columns - 1)) / columns).max(0),
    }
}

/// Measured inputs of the About page's vertical flow, in physical pixels.
#[derive(Clone, Copy, Debug)]
struct AboutFlowInput<'a> {
    available_height: i32,
    width: i32,
    dpi: u32,
    language_row_height: i32,
    check_rows: i32,
    engine_row_height: i32,
    threads_row_height: i32,
    help_text_height: i32,
    credits_text_height: i32,
    button_caption_widths: &'a [i32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AboutButtonPlacement {
    x: i32,
    y: i32,
    width: i32,
}

/// Vertical positions relative to the page rectangle.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AboutFlowLayout {
    version_y: i32,
    description_y: i32,
    description_height: i32,
    language_y: i32,
    check_y: i32,
    check_step: i32,
    engine_y: i32,
    threads_y: i32,
    help_y: i32,
    help_height: i32,
    credits_y: i32,
    credits_height: i32,
    buttons: Vec<AboutButtonPlacement>,
    button_height: i32,
    bottom: i32,
}

#[derive(Clone, Copy, Debug)]
struct AboutCompaction {
    tight_spacing: bool,
    single_button_row: bool,
    show_help: bool,
    show_description: bool,
}

/// The About page sits above the command bar whose left side shows the boot mode, TPM and
/// Secure Boot status. Longer German and French captions wrapped the fourth button into a
/// second row that covered that status. The original flow is kept whenever it fits; otherwise
/// the least visible compaction that fits is used: one caption-sized button row, then tighter
/// vertical spacing, and only as a last resort the optional settings hint and the one-line
/// description are hidden.
fn about_flow_layout(input: &AboutFlowInput<'_>) -> AboutFlowLayout {
    const STEPS: [AboutCompaction; 6] = [
        AboutCompaction {
            tight_spacing: false,
            single_button_row: false,
            show_help: true,
            show_description: true,
        },
        AboutCompaction {
            tight_spacing: false,
            single_button_row: true,
            show_help: true,
            show_description: true,
        },
        AboutCompaction {
            tight_spacing: true,
            single_button_row: false,
            show_help: true,
            show_description: true,
        },
        AboutCompaction {
            tight_spacing: true,
            single_button_row: true,
            show_help: true,
            show_description: true,
        },
        AboutCompaction {
            tight_spacing: true,
            single_button_row: true,
            show_help: false,
            show_description: true,
        },
        AboutCompaction {
            tight_spacing: true,
            single_button_row: true,
            show_help: false,
            show_description: false,
        },
    ];
    let mut layout = about_flow_layout_with(input, STEPS[0]);
    for step in &STEPS[1..] {
        if layout.bottom <= input.available_height {
            break;
        }
        layout = about_flow_layout_with(input, *step);
    }
    layout
}

fn about_flow_layout_with(
    input: &AboutFlowInput<'_>,
    compaction: AboutCompaction,
) -> AboutFlowLayout {
    // Same truncating scale as the page layout, so the uncompacted flow is unchanged.
    let s = |value: i32| value * input.dpi as i32 / 96;
    let width = input.width.max(0);
    let tight = compaction.tight_spacing;
    let gap = if tight { s(4) } else { s(8) };
    let version_y = if tight { s(32) } else { s(36) };
    let description_y = version_y + if tight { s(32) } else { s(38) };
    let description_height = match (compaction.show_description, tight) {
        (false, _) => 0,
        (true, true) => s(24),
        (true, false) => s(28),
    };
    let language_y = if compaction.show_description {
        description_y + description_height + gap
    } else {
        description_y
    };
    let check_y = language_y + input.language_row_height + gap;
    let check_step = s(26) + gap;
    let engine_y = check_y + input.check_rows.max(0) * check_step;
    // Hidden rows (the WIM engine and download-thread rows outside Advanced mode and the removed
    // settings hint) take neither height nor a gap. Reserving them left an empty band of about
    // 80 px at 100 % scale between the last setting and the copyright line.
    let row_extent = |height: i32| if height > 0 { height + gap } else { 0 };
    let threads_y = engine_y + row_extent(input.engine_row_height);
    let help_y = threads_y + row_extent(input.threads_row_height);
    let help_height = match (compaction.show_help && input.help_text_height > 0, tight) {
        (false, _) => 0,
        (true, true) => input.help_text_height,
        (true, false) => input.help_text_height.max(s(48)),
    };
    let credits_y = help_y + row_extent(help_height);
    let credits_height = if tight {
        input.credits_text_height.max(0)
    } else {
        input.credits_text_height.max(s(44))
    };
    let buttons_y = credits_y + credits_height + gap;
    let button_height = s(30);
    let column_gap = s(8);
    let single_row = if compaction.single_button_row {
        about_single_button_row(width, column_gap, input.button_caption_widths, input.dpi)
    } else {
        None
    };
    let buttons: Vec<AboutButtonPlacement> = match single_row {
        Some(row) => row
            .into_iter()
            .map(|(x, width)| AboutButtonPlacement {
                x,
                y: buttons_y,
                width,
            })
            .collect(),
        None => {
            let widest = input
                .button_caption_widths
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
            let grid = about_button_layout(width, input.dpi, widest);
            (0..input.button_caption_widths.len() as i32)
                .map(|index| AboutButtonPlacement {
                    x: (index % grid.columns) * (grid.button_width + column_gap),
                    y: buttons_y + (index / grid.columns) * (button_height + gap),
                    width: grid.button_width,
                })
                .collect()
        }
    };
    let bottom = buttons
        .iter()
        .map(|button| button.y + button_height)
        .max()
        .unwrap_or(credits_y + credits_height);
    AboutFlowLayout {
        version_y,
        description_y,
        description_height,
        language_y,
        check_y,
        check_step,
        engine_y,
        threads_y,
        help_y,
        help_height,
        credits_y,
        credits_height,
        buttons,
        button_height,
        bottom,
    }
}

/// Places every About button on one row when each keeps its caption plus a compact inner
/// padding. Spare width is shared evenly so the row still spans the page.
fn about_single_button_row(
    width: i32,
    gap: i32,
    captions: &[i32],
    dpi: u32,
) -> Option<Vec<(i32, i32)>> {
    let count = i32::try_from(captions.len()).ok()?;
    if count == 0 {
        return Some(Vec::new());
    }
    let padding = ((16_i64 * i64::from(dpi.max(1)) + 48) / 96) as i32;
    let minimum = captions
        .iter()
        .map(|caption| (*caption).max(0) + padding)
        .sum::<i32>()
        + gap * (count - 1);
    if minimum > width {
        return None;
    }
    let spare = width - minimum;
    let mut x = 0;
    Some(
        captions
            .iter()
            .enumerate()
            .map(|(index, caption)| {
                let index = index as i32;
                let share = spare / count + i32::from(index < spare % count);
                let button_width = (*caption).max(0) + padding + share;
                let placement = (x, button_width);
                x += button_width + gap;
                placement
            })
            .collect(),
    )
}

/// Closed ComboBox width that shows the longest item without a trailing empty field: the measured
/// caption, the closed-field painter's 6 px text inset, a short gap before the arrow and the arrow
/// button itself. Never wider than the space beside the label column.
fn combo_width_for_widest_item(widest_item: i32, available: i32, dpi: u32) -> i32 {
    let s = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
    (widest_item.max(0) + s(6) + s(14) + s(20) + s(2))
        .max(s(64))
        .min(available.max(0))
}

unsafe fn fitted_combo_width(combo: HWND, font: HFONT, dpi: u32, available: i32) -> i32 {
    const CB_GETCOUNT_MESSAGE: u32 = 0x0146;
    const CB_GETLBTEXT_MESSAGE: u32 = 0x0148;
    const CB_GETLBTEXTLEN_MESSAGE: u32 = 0x0149;
    let count = SendMessageW(combo, CB_GETCOUNT_MESSAGE, WPARAM(0), LPARAM(0))
        .0
        .max(0) as usize;
    let mut widest = 0;
    for index in 0..count {
        let length = SendMessageW(combo, CB_GETLBTEXTLEN_MESSAGE, WPARAM(index), LPARAM(0)).0;
        if length <= 0 {
            continue;
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let copied = SendMessageW(
            combo,
            CB_GETLBTEXT_MESSAGE,
            WPARAM(index),
            LPARAM(buffer.as_mut_ptr() as isize),
        )
        .0;
        if copied <= 0 {
            continue;
        }
        let text = String::from_utf16_lossy(&buffer[..copied as usize]);
        widest = widest.max(measure_text(combo, font, &text, None).width);
    }
    combo_width_for_widest_item(widest, available, dpi)
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn qa_scroll_fixture_is_long_and_deterministic() {
        let rows = qa_long_hardware_rows();
        assert_eq!(rows.len(), 120);
        assert_eq!(rows[0].item, "Scrollable hardware row 001");
        assert_eq!(rows[119].value, "Deterministic preview value 120");
    }

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
            (
                ID_ABOUT_AUTOMATION_EXPORT,
                InfoIntent::ToggleAutomationExport,
            ),
            (ID_ABOUT_WIM_ENGINE, InfoIntent::SelectWimEngine),
            (ID_ABOUT_DOWNLOAD_THREADS, InfoIntent::SelectDownloadThreads),
        ] {
            assert_eq!(AboutPage::command_intent(command), Some(intent));
        }
    }

    #[test]
    fn about_buttons_share_one_compact_responsive_grid() {
        let compact = about_button_layout(300, 96, 0);
        assert_eq!(compact.columns, 2);
        assert_eq!(compact.button_width, 146);
        let high_dpi = about_button_layout(600, 192, 0);
        assert_eq!(high_dpi.columns, 2);
        assert_eq!(high_dpi.button_width, 292);
        let narrow = about_button_layout(180, 192, 0);
        assert_eq!(narrow.columns, 1);
        assert_eq!(narrow.button_width, 180);
    }

    fn about_flow_fixture(available_height: i32, captions: &[i32]) -> AboutFlowInput<'_> {
        // Geometry measured from the 150 % German report: 940 px content width and 634 px
        // between the page header and the command bar.
        AboutFlowInput {
            available_height,
            width: 940,
            dpi: 144,
            language_row_height: 35,
            check_rows: 2,
            engine_row_height: 35,
            threads_row_height: 35,
            help_text_height: 74,
            credits_text_height: 74,
            button_caption_widths: captions,
        }
    }

    #[test]
    fn about_flow_keeps_the_original_geometry_whenever_it_fits() {
        let captions = [120, 110, 130, 140];
        let layout = about_flow_layout(&about_flow_fixture(2_000, &captions));
        let s = |value: i32| value * 144 / 96;
        assert_eq!(layout.version_y, s(36));
        assert_eq!(layout.description_y, s(36) + s(38));
        assert_eq!(layout.description_height, s(28));
        assert_eq!(layout.language_y, layout.description_y + s(28) + s(8));
        assert_eq!(layout.check_step, s(26) + s(8));
        assert_eq!(layout.help_height, 74);
        let grid = about_button_layout(940, 144, 140);
        assert!(layout
            .buttons
            .iter()
            .all(|button| button.width == grid.button_width));
        assert_eq!(
            layout.buttons[0].y,
            layout.credits_y + layout.credits_height + s(8)
        );
    }

    #[test]
    fn german_about_buttons_move_into_one_row_instead_of_the_status_bar() {
        let captions = [153, 203, 194, 230];
        let input = about_flow_fixture(634, &captions);
        let original = about_flow_layout_with(
            &input,
            AboutCompaction {
                tight_spacing: false,
                single_button_row: false,
                show_help: true,
                show_description: true,
            },
        );
        assert!(original.bottom > 634, "fixture must reproduce the overlap");

        let layout = about_flow_layout(&input);
        assert!(
            layout.bottom <= 634,
            "bottom {} overlaps the command bar",
            layout.bottom
        );
        assert!(layout
            .buttons
            .iter()
            .all(|button| button.y == layout.buttons[0].y));
        assert_eq!(layout.help_height, 74);
        let last = layout.buttons.last().expect("four About buttons");
        assert_eq!(last.x + last.width, 940);
        for (button, caption) in layout.buttons.iter().zip(captions) {
            assert!(button.width >= caption + 24);
        }
        for pair in layout.buttons.windows(2) {
            assert!(pair[0].x + pair[0].width < pair[1].x);
        }
    }

    #[test]
    fn long_french_captions_tighten_spacing_before_hiding_anything() {
        let captions = [211, 176, 176, 284];
        let layout = about_flow_layout(&about_flow_fixture(634, &captions));
        assert!(layout.bottom <= 634, "bottom {}", layout.bottom);
        assert!(layout.help_height > 0);
        assert!(layout.description_height > 0);
    }

    #[test]
    fn only_a_very_short_page_hides_the_optional_settings_hint() {
        let captions = [211, 176, 176, 284];
        let layout = about_flow_layout(&about_flow_fixture(540, &captions));
        assert_eq!(layout.help_height, 0);
        assert!(layout.description_height > 0);
        assert!(layout.bottom <= 540, "bottom {}", layout.bottom);
    }

    #[test]
    fn copyright_follows_the_last_visible_setting_when_optional_rows_are_hidden() {
        let input = AboutFlowInput {
            available_height: 2_000,
            width: 900,
            dpi: 96,
            language_row_height: 54,
            check_rows: 1,
            engine_row_height: 0,
            threads_row_height: 0,
            help_text_height: 0,
            credits_text_height: 20,
            button_caption_widths: &[],
        };
        let layout = about_flow_layout(&input);
        assert_eq!(layout.help_height, 0);
        assert_eq!(layout.credits_y, layout.check_y + layout.check_step);
    }

    #[test]
    fn drop_down_width_follows_the_longest_item_and_the_available_space() {
        // 56 px caption + 6 inset + 14 gap + 20 arrow + 2 frame.
        assert_eq!(combo_width_for_widest_item(56, 600, 96), 98);
        assert_eq!(
            combo_width_for_widest_item(56, 600, 192),
            56 + 12 + 28 + 40 + 4
        );
        assert_eq!(combo_width_for_widest_item(400, 300, 96), 300);
        assert_eq!(combo_width_for_widest_item(8, 600, 96), 64);
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
    fn unknown_link_speed_is_never_rendered_as_zero_mbps() {
        let unknown = network_link_status_text("已连接", 0);
        assert!(unknown.contains(&crate::tr!("未知")));
        assert!(!unknown.contains("0 Mbps"));
        assert_eq!(
            network_link_status_text("已连接", 1_000_000_000),
            "已连接 | 1000 Mbps"
        );
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
