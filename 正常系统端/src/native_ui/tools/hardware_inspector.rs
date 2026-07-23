//! Modeless, read-only detailed hardware inspector.

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, BS_OWNERDRAW, WS_BORDER, WS_TABSTOP,
};

use crate::core::hardware_info::format_bytes;
use crate::core::hardware_inspector::HardwareInspectorSnapshot;
use crate::native_ui::controls::{child, wide};
use crate::native_ui::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use crate::native_ui::theme::apply_list_view_theme;

const FIRST_NAV_ID: u16 = 65_000;
const LIST_ID: u16 = 65_020;
const STATUS_ID: u16 = 65_021;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareInspectorIntent {
    Refresh,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspectorSection {
    Overview,
    Cpu,
    Mainboard,
    Memory,
    Graphics,
    Storage,
}

impl InspectorSection {
    const ALL: [Self; 6] = [
        Self::Overview,
        Self::Cpu,
        Self::Mainboard,
        Self::Memory,
        Self::Graphics,
        Self::Storage,
    ];

    const fn command_id(self) -> u16 {
        FIRST_NAV_ID + self as u16
    }

    fn caption(self) -> String {
        match self {
            Self::Overview => crate::tr!("概览"),
            Self::Cpu => crate::tr!("处理器"),
            Self::Mainboard => crate::tr!("主板与 BIOS"),
            Self::Memory => crate::tr!("内存与插槽"),
            Self::Graphics => crate::tr!("显卡"),
            Self::Storage => crate::tr!("存储设备"),
        }
    }
}

#[derive(Clone, Debug)]
struct DetailRow {
    group: String,
    item: String,
    value: String,
}

impl DetailRow {
    fn new(group: impl Into<String>, item: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            group: group.into(),
            item: item.into(),
            value: value.into(),
        }
    }
}

pub struct NativeHardwareInspectorDialog {
    shell: DialogShell,
    nav: [HWND; 6],
    list: HWND,
    status: HWND,
    section: InspectorSection,
    snapshot: Option<HardwareInspectorSnapshot>,
    last_layout: (i32, i32, u32),
}

impl NativeHardwareInspectorDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let spec = DialogSpec {
            window_title: crate::tr!("详细硬件检测"),
            title: crate::tr!("详细硬件检测"),
            description: crate::tr!(
                "通过 Windows API、SMBIOS 固件表和 CPUID 读取当前计算机的详细硬件信息。"
            ),
            width: 900,
            height: 650,
            buttons: DialogButtons {
                primary: crate::tr!("刷新"),
                secondary: None,
                cancel: Some(crate::tr!("关闭")),
            },
        };
        let mut shell = DialogShell::create(owner, spec)?;
        shell.set_primary_closes(false);
        let parent = shell.content();
        let mut nav = [HWND::default(); 6];
        for (index, section) in InspectorSection::ALL.into_iter().enumerate() {
            nav[index] = child(
                parent,
                w!("BUTTON"),
                &section.caption(),
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                section.command_id(),
            )?;
        }
        let list = child(
            parent,
            w!("SysListView32"),
            "",
            (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
            LIST_ID,
        )?;
        let _ = SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
        );
        insert_columns(list);
        let status = child(
            parent,
            w!("STATIC"),
            &crate::tr!("正在读取硬件信息..."),
            0,
            STATUS_ID,
        )?;
        let mut dialog = Self {
            shell,
            nav,
            list,
            status,
            section: InspectorSection::Overview,
            snapshot: None,
            last_layout: (-1, -1, 0),
        };
        dialog.layout();
        dialog.update_nav();
        dialog.set_loading();
        Ok(dialog)
    }

    pub fn owns_command(command_id: u16) -> bool {
        (FIRST_NAV_ID..FIRST_NAV_ID + InspectorSection::ALL.len() as u16).contains(&command_id)
    }

    pub unsafe fn handle_command(&mut self, command_id: u16) {
        let Some(index) = command_id.checked_sub(FIRST_NAV_ID).map(usize::from) else {
            return;
        };
        let Some(section) = InspectorSection::ALL.get(index).copied() else {
            return;
        };
        self.section = section;
        self.update_nav();
        self.render();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.shell.show_modeless();
    }

    pub unsafe fn activate_if_visible(&self) -> bool {
        self.shell.activate_if_visible()
    }

    pub unsafe fn refresh_layout(&mut self) {
        self.layout();
    }

    pub unsafe fn set_loading(&mut self) {
        self.shell.set_primary_enabled(false);
        set_text(self.status, &crate::tr!("正在读取硬件信息..."));
        replace_rows(
            self.list,
            &[DetailRow::new(
                crate::tr!("状态"),
                crate::tr!("硬件检测"),
                crate::tr!("正在读取硬件信息..."),
            )],
        );
    }

    pub unsafe fn apply_snapshot(&mut self, result: Result<HardwareInspectorSnapshot, String>) {
        self.shell.set_primary_enabled(true);
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                set_text(self.status, &crate::tr!("硬件信息读取完成。"));
                self.render();
            }
            Err(error) => {
                self.snapshot = None;
                set_text(self.status, &crate::tr!("读取硬件信息失败：{}", error));
                replace_rows(
                    self.list,
                    &[DetailRow::new(
                        crate::tr!("状态"),
                        crate::tr!("读取失败"),
                        error,
                    )],
                );
            }
        }
    }

    pub fn take_intent(&mut self) -> Option<HardwareInspectorIntent> {
        match self.shell.take_result()? {
            DialogResult::Primary => Some(HardwareInspectorIntent::Refresh),
            DialogResult::Cancel | DialogResult::Secondary => Some(HardwareInspectorIntent::Close),
        }
    }

    pub unsafe fn relocalize(&mut self) {
        self.shell.relocalize(
            &crate::tr!("详细硬件检测"),
            &crate::tr!("详细硬件检测"),
            &crate::tr!("通过 Windows API、SMBIOS 固件表和 CPUID 读取当前计算机的详细硬件信息。"),
            &crate::tr!("刷新"),
        );
        self.update_nav();
        update_columns(self.list);
        if self.snapshot.is_some() {
            set_text(self.status, &crate::tr!("硬件信息读取完成。"));
            self.render();
        } else {
            set_text(self.status, &crate::tr!("正在读取硬件信息..."));
        }
    }

    unsafe fn layout(&mut self) {
        let parent = self.shell.content();
        let mut rect = RECT::default();
        let _ = GetClientRect(parent, &mut rect);
        let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(parent).max(96);
        let scale = |value: i32| value * dpi as i32 / 96;
        let width = (rect.right - rect.left).max(0);
        let height = (rect.bottom - rect.top).max(0);
        if self.last_layout == (width, height, dpi) {
            return;
        }
        self.last_layout = (width, height, dpi);
        let nav_width = scale(150).min(width / 3);
        let gap = scale(10);
        let button_height = scale(38);
        for (index, button) in self.nav.iter().copied().enumerate() {
            let _ = MoveWindow(
                button,
                0,
                index as i32 * (button_height + scale(7)),
                nav_width,
                button_height,
                true,
            );
        }
        let list_x = nav_width + gap;
        let status_height = scale(24);
        let _ = MoveWindow(
            self.list,
            list_x,
            0,
            (width - list_x).max(0),
            (height - status_height - gap).max(scale(120)),
            true,
        );
        let _ = MoveWindow(
            self.status,
            list_x,
            (height - status_height).max(0),
            (width - list_x).max(0),
            status_height,
            true,
        );
        let list_width = (width - list_x).max(0);
        for (index, column_width) in [
            (list_width * 20 / 100).max(scale(110)),
            (list_width * 26 / 100).max(scale(150)),
            (list_width * 54 / 100).max(scale(280)),
        ]
        .into_iter()
        .enumerate()
        {
            let _ = SendMessageW(
                self.list,
                LVM_SETCOLUMNWIDTH,
                WPARAM(index),
                LPARAM(column_width as isize),
            );
        }
        let palette = self.shell.palette();
        let _ = apply_list_view_theme(self.list, palette);
    }

    unsafe fn update_nav(&mut self) {
        for (button, section) in self.nav.into_iter().zip(InspectorSection::ALL) {
            set_text(button, &section.caption());
            let _ = EnableWindow(button, true);
        }
        self.shell
            .set_selected_navigation_command(self.section.command_id());
    }

    unsafe fn render(&self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let rows = match self.section {
            InspectorSection::Overview => overview_rows(snapshot),
            InspectorSection::Cpu => cpu_rows(snapshot),
            InspectorSection::Mainboard => mainboard_rows(snapshot),
            InspectorSection::Memory => memory_rows(snapshot),
            InspectorSection::Graphics => graphics_rows(snapshot),
            InspectorSection::Storage => storage_rows(snapshot),
        };
        replace_rows(self.list, &collapse_repeated_groups(rows));
    }
}

fn collapse_repeated_groups(mut rows: Vec<DetailRow>) -> Vec<DetailRow> {
    let mut previous = String::new();
    for row in &mut rows {
        if row.group == previous {
            row.group.clear();
        } else {
            previous.clone_from(&row.group);
        }
    }
    rows
}

fn overview_rows(snapshot: &HardwareInspectorSnapshot) -> Vec<DetailRow> {
    let info = &snapshot.base;
    vec![
        DetailRow::new(
            crate::tr!("系统"),
            crate::tr!("计算机"),
            join_nonempty(&[&info.computer_manufacturer, &info.computer_model]),
        ),
        DetailRow::new(
            crate::tr!("系统"),
            crate::tr!("序列号"),
            display_value(&info.system_serial_number),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("型号"),
            display_value(first_nonempty(&snapshot.cpuid.brand, &info.cpu.name)),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("核心 / 逻辑处理器"),
            format!("{} / {}", info.cpu.cores, info.cpu.logical_processors),
        ),
        DetailRow::new(
            crate::tr!("内存"),
            crate::tr!("总容量"),
            format_bytes(info.memory.total_physical),
        ),
        DetailRow::new(
            crate::tr!("内存"),
            crate::tr!("已安装模块"),
            snapshot.smbios.memory_modules.len().to_string(),
        ),
        DetailRow::new(
            crate::tr!("显卡"),
            crate::tr!("适配器数量"),
            snapshot.graphics.len().max(info.gpus.len()).to_string(),
        ),
        DetailRow::new(
            crate::tr!("存储设备"),
            crate::tr!("物理磁盘数量"),
            snapshot.disks.len().to_string(),
        ),
    ]
}

fn cpu_rows(snapshot: &HardwareInspectorSnapshot) -> Vec<DetailRow> {
    let cpu = &snapshot.base.cpu;
    let cpuid = &snapshot.cpuid;
    vec![
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("型号"),
            display_value(first_nonempty(&cpuid.brand, &cpu.name)),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("CPUID 厂商"),
            display_value(first_nonempty(&cpuid.vendor, &cpu.manufacturer)),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            "Family / Model / Stepping",
            format!("{} / {} / {}", cpuid.family, cpuid.model, cpuid.stepping),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("微架构 / 代号"),
            display_value(&cpuid.microarchitecture),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("制程"),
            display_value(&cpuid.process_node),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("架构"),
            display_value(&cpu.architecture),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("核心 / 逻辑处理器"),
            format!("{} / {}", cpu.cores, cpu.logical_processors),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("当前 / 最大频率"),
            format!(
                "{} MHz / {} MHz",
                cpu.current_clock_speed, cpu.max_clock_speed
            ),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("L2 / L3 缓存"),
            format!(
                "{} / {}",
                format_l2_cache(cpu.cores, cpu.l2_cache_size, cpuid.l2_cache_bytes),
                format_cache(cpu.l3_cache_size, cpuid.l3_cache_bytes)
            ),
        ),
        DetailRow::new(
            crate::tr!("处理器"),
            crate::tr!("指令集"),
            if cpuid.features.is_empty() {
                crate::tr!("无法读取")
            } else {
                cpuid.features.join(", ")
            },
        ),
    ]
}

fn mainboard_rows(snapshot: &HardwareInspectorSnapshot) -> Vec<DetailRow> {
    let board = &snapshot.base.motherboard;
    let bios = &snapshot.base.bios;
    let smbios = &snapshot.smbios;
    vec![
        DetailRow::new(
            crate::tr!("主板"),
            crate::tr!("制造商"),
            display_value(first_nonempty(
                &smbios.board_manufacturer,
                &board.manufacturer,
            )),
        ),
        DetailRow::new(
            crate::tr!("主板"),
            crate::tr!("型号"),
            display_value(first_nonempty(&smbios.board_product, &board.product)),
        ),
        DetailRow::new(
            crate::tr!("主板"),
            crate::tr!("版本"),
            display_value(first_nonempty(&smbios.board_version, &board.version)),
        ),
        DetailRow::new(
            crate::tr!("主板"),
            crate::tr!("序列号"),
            display_value(first_nonempty(&smbios.board_serial, &board.serial_number)),
        ),
        DetailRow::new(
            "BIOS",
            crate::tr!("制造商"),
            display_value(first_nonempty(&smbios.bios_vendor, &bios.manufacturer)),
        ),
        DetailRow::new(
            "BIOS",
            crate::tr!("固件版本"),
            display_value(first_nonempty(&smbios.bios_version, &bios.version)),
        ),
        DetailRow::new(
            "BIOS",
            crate::tr!("发布日期"),
            display_value(first_nonempty(&smbios.bios_date, &bios.release_date)),
        ),
        DetailRow::new(
            "SMBIOS",
            crate::tr!("版本"),
            if smbios.major_version == 0 {
                display_value(&bios.smbios_version)
            } else {
                format!("{}.{}", smbios.major_version, smbios.minor_version)
            },
        ),
    ]
}

fn memory_rows(snapshot: &HardwareInspectorSnapshot) -> Vec<DetailRow> {
    let memory = &snapshot.base.memory;
    let mut rows = vec![
        DetailRow::new(
            crate::tr!("内存"),
            crate::tr!("总容量"),
            format_bytes(memory.total_physical),
        ),
        DetailRow::new(
            crate::tr!("内存"),
            crate::tr!("可用容量"),
            format_bytes(memory.available_physical),
        ),
        DetailRow::new(
            crate::tr!("内存"),
            crate::tr!("使用率"),
            format!("{}%", memory.memory_load),
        ),
        DetailRow::new(
            crate::tr!("内存"),
            crate::tr!("插槽数量"),
            memory.slot_count.to_string(),
        ),
    ];
    if snapshot.smbios.memory_modules.is_empty() {
        rows.push(DetailRow::new(
            crate::tr!("内存模块"),
            crate::tr!("状态"),
            crate::tr!("固件未提供可用的 SMBIOS 内存模块信息。"),
        ));
        return rows;
    }
    for (index, module) in snapshot.smbios.memory_modules.iter().enumerate() {
        let group = crate::tr!("内存模块 {}", index + 1);
        rows.extend([
            DetailRow::new(
                &group,
                crate::tr!("位置 / Bank"),
                join_nonempty(&[&module.locator, &module.bank]),
            ),
            DetailRow::new(
                &group,
                crate::tr!("容量"),
                if module.size_bytes == 0 {
                    crate::tr!("无法读取")
                } else {
                    format_bytes(module.size_bytes)
                },
            ),
            DetailRow::new(
                &group,
                crate::tr!("类型"),
                display_value(&module.memory_type),
            ),
            DetailRow::new(
                &group,
                crate::tr!("标称 / 配置速度"),
                format!(
                    "{} MT/s / {} MT/s",
                    module.speed_mts, module.configured_speed_mts
                ),
            ),
            DetailRow::new(
                &group,
                crate::tr!("制造商"),
                display_value(&module.manufacturer),
            ),
            DetailRow::new(
                &group,
                crate::tr!("部件号"),
                display_value(&module.part_number),
            ),
            DetailRow::new(
                &group,
                crate::tr!("序列号"),
                display_value(&module.serial_number),
            ),
        ]);
    }
    rows
}

fn graphics_rows(snapshot: &HardwareInspectorSnapshot) -> Vec<DetailRow> {
    if snapshot.graphics.is_empty() {
        return snapshot
            .base
            .gpus
            .iter()
            .enumerate()
            .flat_map(|(index, gpu)| {
                let group = crate::tr!("显卡 {}", index + 1);
                [
                    DetailRow::new(&group, crate::tr!("型号"), display_value(&gpu.name)),
                    DetailRow::new(
                        &group,
                        crate::tr!("驱动版本"),
                        display_value(&gpu.driver_version),
                    ),
                    DetailRow::new(&group, crate::tr!("显存"), format_bytes(gpu.video_memory)),
                ]
            })
            .collect();
    }
    snapshot
        .graphics
        .iter()
        .enumerate()
        .flat_map(|(index, gpu)| {
            let group = crate::tr!("显卡 {}", index + 1);
            [
                DetailRow::new(&group, crate::tr!("型号"), display_value(&gpu.name)),
                DetailRow::new(
                    &group,
                    crate::tr!("核心代号 / 架构"),
                    display_value(&gpu.architecture),
                ),
                DetailRow::new(&group, crate::tr!("制程"), display_value(&gpu.process_node)),
                DetailRow::new(
                    &group,
                    crate::tr!("核心配置"),
                    display_value(&gpu.core_configuration),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("设备 ID"),
                    format!("{:04X}:{:04X}", gpu.vendor_id, gpu.device_id),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("子系统 ID"),
                    format!("{:08X}", gpu.subsystem_id),
                ),
                DetailRow::new(&group, crate::tr!("修订版本"), gpu.revision.to_string()),
                DetailRow::new(
                    &group,
                    crate::tr!("专用显存"),
                    format_bytes(gpu.dedicated_video_memory),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("共享系统内存"),
                    format_bytes(gpu.shared_system_memory),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("软件适配器"),
                    yes_no(gpu.software_adapter),
                ),
            ]
        })
        .collect()
}

fn format_cache(wmi_kib: u32, cpuid_bytes: u64) -> String {
    if wmi_kib != 0 {
        format!("{wmi_kib} KiB")
    } else if cpuid_bytes != 0 {
        format_bytes(cpuid_bytes)
    } else {
        crate::tr!("无法读取")
    }
}

fn format_l2_cache(cores: u32, wmi_kib: u32, cpuid_bytes_per_core: u64) -> String {
    if wmi_kib != 0 {
        return format!("{wmi_kib} KiB");
    }
    if cpuid_bytes_per_core == 0 {
        return crate::tr!("无法读取");
    }
    if cores > 1 {
        format!("{cores} × {}", format_bytes(cpuid_bytes_per_core))
    } else {
        format_bytes(cpuid_bytes_per_core)
    }
}

fn storage_rows(snapshot: &HardwareInspectorSnapshot) -> Vec<DetailRow> {
    if snapshot.disks.is_empty() {
        return vec![DetailRow::new(
            crate::tr!("存储设备"),
            crate::tr!("状态"),
            crate::tr!("没有读取到物理磁盘。"),
        )];
    }
    snapshot
        .disks
        .iter()
        .flat_map(|storage| {
            let disk = &storage.disk;
            let group = format!(
                "PhysicalDrive{} · {}",
                disk.disk_index,
                display_value(&disk.model)
            );
            let mut rows = vec![
                DetailRow::new(&group, crate::tr!("型号"), display_value(&disk.model)),
                DetailRow::new(&group, crate::tr!("总容量"), format_bytes(disk.size)),
                DetailRow::new(
                    &group,
                    crate::tr!("序列号"),
                    display_value(&disk.serial_number),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("固件版本"),
                    display_value(&disk.firmware_revision),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("接口类型"),
                    display_value(&disk.interface_type),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("介质类型"),
                    display_value(&disk.media_type),
                ),
                DetailRow::new(
                    &group,
                    crate::tr!("分区表"),
                    display_value(&disk.partition_style),
                ),
                DetailRow::new(&group, crate::tr!("分区数量"), disk.partitions.to_string()),
                DetailRow::new(&group, "TRIM", optional_yes_no(storage.trim_enabled)),
                DetailRow::new(
                    &group,
                    crate::tr!("寻道惩罚"),
                    optional_yes_no(storage.incurs_seek_penalty),
                ),
            ];
            if let Some(health) = &storage.nvme_health {
                rows.extend([
                    DetailRow::new(
                        &group,
                        crate::tr!("健康度"),
                        format!("{}%", health.health_percentage),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("温度"),
                        health.temperature_celsius.map_or_else(
                            || crate::tr!("无法读取"),
                            |temperature| format!("{temperature} °C"),
                        ),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("累计读取"),
                        format_large_bytes(health.data_read_bytes),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("累计写入"),
                        format_large_bytes(health.data_written_bytes),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("通电时间"),
                        crate::tr!("{} 小时", health.power_on_hours),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("通电次数"),
                        health.power_cycles.to_string(),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("非正常关机"),
                        health.unsafe_shutdowns.to_string(),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("介质错误"),
                        health.media_errors.to_string(),
                    ),
                    DetailRow::new(
                        &group,
                        crate::tr!("严重警告标志"),
                        format!("0x{:02X}", health.critical_warning),
                    ),
                ]);
            } else {
                rows.push(DetailRow::new(
                    &group,
                    crate::tr!("S.M.A.R.T. 健康属性"),
                    crate::tr!("设备没有通过 Windows 标准 NVMe 协议通道返回健康日志。"),
                ));
            }
            rows
        })
        .collect()
}

fn format_large_bytes(bytes: u128) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn first_nonempty<'a>(first: &'a str, second: &'a str) -> &'a str {
    if first.trim().is_empty() {
        second
    } else {
        first
    }
}

fn join_nonempty(values: &[&str]) -> String {
    let joined = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    display_value(&joined)
}

fn display_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("unknown") {
        crate::tr!("无法读取")
    } else {
        value.to_owned()
    }
}

fn yes_no(value: bool) -> String {
    if value {
        crate::tr!("是")
    } else {
        crate::tr!("否")
    }
}

fn optional_yes_no(value: Option<bool>) -> String {
    value.map_or_else(|| crate::tr!("无法读取"), yes_no)
}

unsafe fn insert_columns(list: HWND) {
    for (index, (title, width)) in [
        (crate::tr!("类别"), 140),
        (crate::tr!("项目"), 180),
        (crate::tr!("值"), 420),
    ]
    .into_iter()
    .enumerate()
    {
        let mut title = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: width,
            pszText: PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            WPARAM(index),
            LPARAM(&mut column as *mut LVCOLUMNW as isize),
        );
    }
}

unsafe fn update_columns(list: HWND) {
    for (index, title) in [crate::tr!("类别"), crate::tr!("项目"), crate::tr!("值")]
        .into_iter()
        .enumerate()
    {
        let mut title = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT,
            pszText: PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            0x1060, // LVM_SETCOLUMNW
            WPARAM(index),
            LPARAM(&mut column as *mut LVCOLUMNW as isize),
        );
    }
}

unsafe fn replace_rows(list: HWND, rows: &[DetailRow]) {
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    for (row_index, row) in rows.iter().enumerate() {
        for (column, value) in [&row.group, &row.item, &row.value].into_iter().enumerate() {
            let mut value = wide(value);
            let mut item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: row_index as i32,
                iSubItem: column as i32,
                pszText: PWSTR(value.as_mut_ptr()),
                ..Default::default()
            };
            let message = if column == 0 {
                LVM_INSERTITEMW
            } else {
                0x104c // LVM_SETITEMTEXTW
            };
            let _ = SendMessageW(
                list,
                message,
                WPARAM(0),
                LPARAM(&mut item as *mut LVITEMW as isize),
            );
        }
    }
    let _ = RedrawWindow(
        list,
        None,
        None,
        RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_values_are_never_presented_as_valid_inventory() {
        assert_eq!(display_value(""), crate::tr!("无法读取"));
        assert_eq!(display_value("Unknown"), crate::tr!("无法读取"));
        assert_eq!(display_value("RTX 4090"), "RTX 4090");
    }

    #[test]
    fn tool_command_ids_are_private_and_contiguous() {
        for (index, section) in InspectorSection::ALL.into_iter().enumerate() {
            assert_eq!(section.command_id(), FIRST_NAV_ID + index as u16);
            assert!(NativeHardwareInspectorDialog::owns_command(
                section.command_id()
            ));
        }
        assert!(!NativeHardwareInspectorDialog::owns_command(
            FIRST_NAV_ID - 1
        ));
    }

    #[test]
    fn consecutive_device_groups_are_only_shown_once() {
        let rows = collapse_repeated_groups(vec![
            DetailRow::new("显卡 1", "型号", "GPU"),
            DetailRow::new("显卡 1", "制程", "4 nm"),
            DetailRow::new("显卡 2", "型号", "iGPU"),
            DetailRow::new("显卡 2", "制程", "4 nm"),
        ]);
        assert_eq!(rows[0].group, "显卡 1");
        assert!(rows[1].group.is_empty());
        assert_eq!(rows[2].group, "显卡 2");
        assert!(rows[3].group.is_empty());
    }
}
