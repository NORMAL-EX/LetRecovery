//! 一键分区核心模块
//!
//! 提供磁盘分区的底层操作功能，使用 diskpart 和 Windows API 实现

use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    Win32::System::IO::DeviceIoControl,
    Win32::System::Ioctl::{
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, PARTITION_STYLE_GPT,
        PARTITION_STYLE_MBR, PARTITION_STYLE_RAW,
    },
};

/// IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS 常量
/// CTL_CODE(IOCTL_VOLUME_BASE, 0, METHOD_BUFFERED, FILE_ANY_ACCESS)
/// IOCTL_VOLUME_BASE = 0x56 ('V'), 所以值为 (0x56 << 16) | (0 << 14) | (0 << 2) | 0 = 0x00560000
#[cfg(windows)]
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x00560000;

use crate::utils::cmd::create_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

use super::disk::PartitionStyle;
use super::system_info::BootMode;

/// 获取 diskpart 可执行文件路径
fn get_diskpart_path() -> String {
    let builtin_diskpart = get_bin_dir().join("diskpart").join("diskpart.exe");
    if builtin_diskpart.exists() {
        builtin_diskpart.to_string_lossy().to_string()
    } else {
        "diskpart.exe".to_string()
    }
}

/// 物理磁盘信息
#[derive(Debug, Clone)]
pub struct PhysicalDisk {
    /// 磁盘编号
    pub disk_number: u32,
    /// 磁盘大小（字节）
    pub size_bytes: u64,
    /// 磁盘型号/名称
    pub model: String,
    /// 分区表类型
    pub partition_style: PartitionStyle,
    /// 是否已初始化
    pub is_initialized: bool,
    /// 磁盘上的分区列表
    pub partitions: Vec<DiskPartitionInfo>,
    /// 未分配空间（字节）
    pub unallocated_bytes: u64,
}

impl PhysicalDisk {
    /// 获取磁盘大小（GB，保留1位小数）
    pub fn size_gb(&self) -> f64 {
        (self.size_bytes as f64 / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0
    }

    /// 获取已分配空间（字节）
    pub fn allocated_bytes(&self) -> u64 {
        self.partitions.iter().map(|p| p.size_bytes).sum()
    }

    /// 获取显示名称
    pub fn display_name(&self) -> String {
        if self.model.is_empty() {
            format!("磁盘 {} ({:.1} GB)", self.disk_number, self.size_gb())
        } else {
            format!(
                "磁盘 {} - {} ({:.1} GB)",
                self.disk_number,
                self.model,
                self.size_gb()
            )
        }
    }
}

/// 磁盘上的分区信息
#[derive(Debug, Clone)]
pub struct DiskPartitionInfo {
    /// 分区编号
    pub partition_number: u32,
    /// 分区大小（字节）
    pub size_bytes: u64,
    /// 分区偏移量（字节）
    pub offset_bytes: u64,
    /// 盘符（如果有）
    pub drive_letter: Option<char>,
    /// 卷标
    pub label: String,
    /// 文件系统类型
    pub file_system: String,
    /// 是否为 ESP 分区（EFI 系统分区）
    pub is_esp: bool,
    /// 是否为 MSR 分区（微软保留分区）
    pub is_msr: bool,
    /// 是否为恢复分区
    pub is_recovery: bool,
    /// 分区类型 GUID（GPT）或类型 ID（MBR）
    pub partition_type: String,
    /// 已使用空间（字节）
    pub used_bytes: u64,
    /// 空闲空间（字节）
    pub free_bytes: u64,
}

impl DiskPartitionInfo {
    /// 获取分区大小（GB，保留1位小数）
    pub fn size_gb(&self) -> f64 {
        (self.size_bytes as f64 / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0
    }

    /// 获取已使用空间（GB，保留1位小数）
    pub fn used_gb(&self) -> f64 {
        (self.used_bytes as f64 / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0
    }

    /// 获取空闲空间（GB，保留1位小数）
    pub fn free_gb(&self) -> f64 {
        (self.free_bytes as f64 / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0
    }

    /// 获取显示名称
    pub fn display_name(&self) -> String {
        if let Some(letter) = self.drive_letter {
            if self.label.is_empty() {
                format!("{}:", letter)
            } else {
                format!("{}: ({})", letter, self.label)
            }
        } else if self.is_esp {
            "ESP".to_string()
        } else if self.is_msr {
            "MSR".to_string()
        } else if self.is_recovery {
            "恢复分区".to_string()
        } else {
            format!("分区 {}", self.partition_number)
        }
    }
}

/// 用户设计的分区布局
#[derive(Debug, Clone)]
pub struct PartitionLayout {
    /// 分区大小（GB）
    pub size_gb: f64,
    /// 盘符（可选）
    pub drive_letter: Option<char>,
    /// 卷标
    pub label: String,
    /// 是否为 ESP 分区
    pub is_esp: bool,
    /// 文件系统类型
    pub file_system: String,
}

impl Default for PartitionLayout {
    fn default() -> Self {
        Self {
            size_gb: 0.0,
            drive_letter: None,
            label: String::new(),
            is_esp: false,
            file_system: "NTFS".to_string(),
        }
    }
}

/// 一键分区操作结果
#[derive(Debug, Clone)]
pub struct QuickPartitionResult {
    pub success: bool,
    pub message: String,
    pub created_partitions: Vec<String>,
}

/// DISK_GEOMETRY_EX 结构
/// 根据 Windows SDK 定义:
/// struct DISK_GEOMETRY {
///     LARGE_INTEGER Cylinders;        // 8 bytes
///     MEDIA_TYPE    MediaType;        // 4 bytes
///     DWORD         TracksPerCylinder; // 4 bytes
///     DWORD         SectorsPerTrack;   // 4 bytes
///     DWORD         BytesPerSector;    // 4 bytes
/// }; // Total: 24 bytes
/// struct DISK_GEOMETRY_EX {
///     DISK_GEOMETRY Geometry;
///     LARGE_INTEGER DiskSize;         // 8 bytes
///     BYTE          Data[1];
/// };
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct DiskGeometryEx {
    // DISK_GEOMETRY 部分 (24 bytes)
    geometry_cylinders: i64,           // 8 bytes - LARGE_INTEGER 必须在最前面！
    geometry_media_type: u32,          // 4 bytes
    geometry_tracks_per_cylinder: u32, // 4 bytes
    geometry_sectors_per_track: u32,   // 4 bytes
    geometry_bytes_per_sector: u32,    // 4 bytes
    // DISK_GEOMETRY_EX 扩展部分
    disk_size: i64,                    // 8 bytes - 这才是我们需要的磁盘大小
}

/// DRIVE_LAYOUT_INFORMATION_EX 结构头部
#[cfg(windows)]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DriveLayoutInfoExHeader {
    partition_style: u32,
    partition_count: u32,
}

/// PARTITION_INFORMATION_EX 结构（GPT）
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct PartitionInfoExGpt {
    starting_offset: i64,
    partition_length: i64,
    partition_number: u32,
    rewrite_partition: u8,
    is_service_partition: u8,
    _padding: [u8; 2],
    partition_style: u32,
    // GPT specific
    partition_type_guid: [u8; 16],
    partition_id_guid: [u8; 16],
    attributes: u64,
    name: [u16; 36],
}

#[cfg(windows)]
impl Default for PartitionInfoExGpt {
    fn default() -> Self {
        Self {
            starting_offset: 0,
            partition_length: 0,
            partition_number: 0,
            rewrite_partition: 0,
            is_service_partition: 0,
            _padding: [0; 2],
            partition_style: 0,
            partition_type_guid: [0; 16],
            partition_id_guid: [0; 16],
            attributes: 0,
            name: [0; 36],
        }
    }
}

/// PARTITION_INFORMATION_EX 结构（MBR）
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct PartitionInfoExMbr {
    starting_offset: i64,
    partition_length: i64,
    partition_number: u32,
    rewrite_partition: u8,
    is_service_partition: u8,
    _padding: [u8; 2],
    partition_style: u32,
    // MBR specific
    partition_type: u8,
    boot_indicator: u8,
    recognized_partition: u8,
    hidden_sectors: u32,
    _reserved: [u8; 100], // 填充到与 GPT 相同大小
}

#[cfg(windows)]
impl Default for PartitionInfoExMbr {
    fn default() -> Self {
        Self {
            starting_offset: 0,
            partition_length: 0,
            partition_number: 0,
            rewrite_partition: 0,
            is_service_partition: 0,
            _padding: [0; 2],
            partition_style: 0,
            partition_type: 0,
            boot_indicator: 0,
            recognized_partition: 0,
            hidden_sectors: 0,
            _reserved: [0; 100],
        }
    }
}

/// ESP 分区类型 GUID
const ESP_PARTITION_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

/// MSR 分区类型 GUID
const MSR_PARTITION_TYPE_GUID: [u8; 16] = [
    0x16, 0xe3, 0xc9, 0xe3, 0x5c, 0x0b, 0xb8, 0x4d, 0x81, 0x7d, 0xf9, 0x2d, 0xf0, 0x02, 0x15, 0xae,
];

/// Windows 恢复分区类型 GUID
const RECOVERY_PARTITION_TYPE_GUID: [u8; 16] = [
    0xa4, 0xbb, 0x94, 0xde, 0xd1, 0x06, 0x40, 0x4d, 0xa1, 0x6a, 0xbf, 0xd5, 0x01, 0x79, 0xd6, 0xac,
];

/// 获取所有物理磁盘列表
#[cfg(windows)]
pub fn get_physical_disks() -> Vec<PhysicalDisk> {
    let mut disks = Vec::new();

    // 通过尝试打开物理磁盘来枚举
    for disk_num in 0..32 {
        if let Some(disk) = get_disk_info(disk_num) {
            disks.push(disk);
        }
    }

    disks
}

#[cfg(not(windows))]
pub fn get_physical_disks() -> Vec<PhysicalDisk> {
    Vec::new()
}

/// 获取单个磁盘的详细信息
#[cfg(windows)]
fn get_disk_info(disk_number: u32) -> Option<PhysicalDisk> {
    unsafe {
        let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_number);
        let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();

        let handle = CreateFileW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        );

        let handle = match handle {
            Ok(h) => h,
            Err(_) => return None,
        };

        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        // 获取磁盘大小
        let mut geometry = DiskGeometryEx::default();
        let mut bytes_returned: u32 = 0;

        let size_result = DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            None,
            0,
            Some(&mut geometry as *mut _ as *mut _),
            std::mem::size_of::<DiskGeometryEx>() as u32,
            Some(&mut bytes_returned),
            None,
        );

        let size_bytes = if size_result.is_ok() {
            geometry.disk_size as u64
        } else {
            let _ = CloseHandle(handle);
            return None;
        };

        // 获取分区布局信息
        let mut buffer = vec![0u8; 65536]; // 足够大的缓冲区
        let mut bytes_returned: u32 = 0;

        let layout_result = DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
            None,
            0,
            Some(buffer.as_mut_ptr() as *mut _),
            buffer.len() as u32,
            Some(&mut bytes_returned),
            None,
        );

        let _ = CloseHandle(handle);

        let (partition_style, is_initialized, partitions) = if layout_result.is_ok()
            && bytes_returned >= std::mem::size_of::<DriveLayoutInfoExHeader>() as u32
        {
            let header = &*(buffer.as_ptr() as *const DriveLayoutInfoExHeader);

            let style = match header.partition_style {
                x if x == PARTITION_STYLE_MBR.0 as u32 => PartitionStyle::MBR,
                x if x == PARTITION_STYLE_GPT.0 as u32 => PartitionStyle::GPT,
                x if x == PARTITION_STYLE_RAW.0 as u32 => PartitionStyle::Unknown,
                _ => PartitionStyle::Unknown,
            };

            let is_init = style != PartitionStyle::Unknown;

            // 解析分区信息
            let partitions = parse_partition_layout(&buffer, header, style);

            (style, is_init, partitions)
        } else {
            (PartitionStyle::Unknown, false, Vec::new())
        };

        // 计算未分配空间
        let allocated: u64 = partitions.iter().map(|p| p.size_bytes).sum();
        let unallocated = size_bytes.saturating_sub(allocated);

        // 获取磁盘型号
        let model = get_disk_model(disk_number).unwrap_or_default();

        Some(PhysicalDisk {
            disk_number,
            size_bytes,
            model,
            partition_style,
            is_initialized,
            partitions,
            unallocated_bytes: unallocated,
        })
    }
}

/// 解析分区布局信息
#[cfg(windows)]
fn parse_partition_layout(
    buffer: &[u8],
    header: &DriveLayoutInfoExHeader,
    style: PartitionStyle,
) -> Vec<DiskPartitionInfo> {
    let mut partitions = Vec::new();

    // PARTITION_INFORMATION_EX 结构大小固定为 144 字节
    let partition_entry_size = 144;

    // DRIVE_LAYOUT_INFORMATION_EX 头部大小:
    // - PartitionStyle: 4 bytes
    // - PartitionCount: 4 bytes
    // - Union (GPT: 40 bytes, MBR: 8 bytes，但由于对齐，GPT 可能需要更多)
    // 实际上，Windows 中 GPT 的 union 部分是 40 字节，MBR 是 8 字节
    // 但分区数组需要对齐，所以我们使用正确的偏移
    let header_size = if style == PartitionStyle::GPT {
        8 + 40 // DriveLayoutInfoExHeader(8) + DRIVE_LAYOUT_INFORMATION_GPT(40) = 48
    } else {
        8 + 8 // DriveLayoutInfoExHeader(8) + DRIVE_LAYOUT_INFORMATION_MBR(8) = 16
    };

    for i in 0..header.partition_count {
        let offset = header_size + (i as usize * partition_entry_size);
        if offset + partition_entry_size > buffer.len() {
            break;
        }

        let partition_data = &buffer[offset..offset + partition_entry_size];

        // PARTITION_INFORMATION_EX 结构布局:
        // offset 0:  PartitionStyle (4 bytes)
        // offset 4:  padding (4 bytes) - 为了 8 字节对齐
        // offset 8:  StartingOffset (8 bytes, LARGE_INTEGER)
        // offset 16: PartitionLength (8 bytes, LARGE_INTEGER)
        // offset 24: PartitionNumber (4 bytes)
        // offset 28: RewritePartition (1 byte)
        // offset 29: IsServicePartition (1 byte)
        // offset 30: padding (2 bytes)
        // offset 32: Union start (MBR or GPT specific data)

        let starting_offset = i64::from_le_bytes(partition_data[8..16].try_into().unwrap_or([0; 8]));
        let partition_length = i64::from_le_bytes(partition_data[16..24].try_into().unwrap_or([0; 8]));
        let partition_number = u32::from_le_bytes(partition_data[24..28].try_into().unwrap_or([0; 4]));

        // 跳过大小为0的分区
        if partition_length <= 0 {
            continue;
        }

        let (is_esp, is_msr, is_recovery, partition_type) = if style == PartitionStyle::GPT {
            // GPT: 分区类型 GUID 在 union 开始处 (offset 32)
            // PARTITION_INFORMATION_GPT 结构:
            // offset 0 (32): PartitionType GUID (16 bytes)
            // offset 16 (48): PartitionId GUID (16 bytes)
            // offset 32 (64): Attributes (8 bytes)
            // offset 40 (72): Name (72 bytes, 36 wchars)
            let mut type_guid = [0u8; 16];
            type_guid.copy_from_slice(&partition_data[32..48]);

            let is_esp = type_guid == ESP_PARTITION_TYPE_GUID;
            let is_msr = type_guid == MSR_PARTITION_TYPE_GUID;
            let is_recovery = type_guid == RECOVERY_PARTITION_TYPE_GUID;

            let type_str = format!(
                "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                type_guid[3], type_guid[2], type_guid[1], type_guid[0],
                type_guid[5], type_guid[4],
                type_guid[7], type_guid[6],
                type_guid[8], type_guid[9],
                type_guid[10], type_guid[11], type_guid[12], type_guid[13], type_guid[14], type_guid[15]
            );

            (is_esp, is_msr, is_recovery, type_str)
        } else {
            // MBR: 分区类型 ID 在 union 开始处 (offset 32)
            // PARTITION_INFORMATION_MBR 结构:
            // offset 0 (32): PartitionType (1 byte)
            // offset 1 (33): BootIndicator (1 byte)
            // offset 2 (34): RecognizedPartition (1 byte)
            // offset 4 (36): HiddenSectors (4 bytes)
            let type_id = partition_data[32];
            let type_str = format!("0x{:02X}", type_id);
            (false, false, false, type_str)
        };

        // 获取盘符
        let drive_letter = get_drive_letter_for_partition(starting_offset as u64);

        // 获取卷标、文件系统和空间使用信息
        let (label, file_system, used_bytes, free_bytes) = if let Some(letter) = drive_letter {
            get_volume_info(letter)
        } else {
            (String::new(), String::new(), 0, 0)
        };

        partitions.push(DiskPartitionInfo {
            partition_number,
            size_bytes: partition_length as u64,
            offset_bytes: starting_offset as u64,
            drive_letter,
            label,
            file_system,
            is_esp,
            is_msr,
            is_recovery,
            partition_type,
            used_bytes,
            free_bytes,
        });
    }

    // 按偏移量排序
    partitions.sort_by_key(|p| p.offset_bytes);

    partitions
}

/// 根据分区偏移量获取对应的盘符
#[cfg(windows)]
fn get_drive_letter_for_partition(offset: u64) -> Option<char> {
    for letter in b'C'..=b'Z' {
        let c = letter as char;
        let path = format!("{}:\\", c);
        if !Path::new(&path).exists() {
            continue;
        }

        // 检查这个卷的偏移量是否匹配
        if let Some(vol_offset) = get_volume_offset(c) {
            // 允许一些误差（1MB以内）
            if (vol_offset as i64 - offset as i64).unsigned_abs() < 1024 * 1024 {
                return Some(c);
            }
        }
    }
    None
}

/// 获取卷的偏移量
#[cfg(windows)]
fn get_volume_offset(letter: char) -> Option<u64> {
    unsafe {
        let volume_path = format!("\\\\.\\{}:", letter);
        let wide_path: Vec<u16> = volume_path.encode_utf16().chain(std::iter::once(0)).collect();

        let handle = CreateFileW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        );

        let handle = match handle {
            Ok(h) => h,
            Err(_) => return None,
        };

        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        // VOLUME_DISK_EXTENTS 结构
        #[repr(C)]
        struct DiskExtent {
            disk_number: u32,
            starting_offset: i64,
            extent_length: i64,
        }

        #[repr(C)]
        struct VolumeDiskExtents {
            number_of_disk_extents: u32,
            extents: [DiskExtent; 1],
        }

        let mut buffer = [0u8; 256];
        let mut bytes_returned: u32 = 0;

        let result = DeviceIoControl(
            handle,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            0,
            Some(buffer.as_mut_ptr() as *mut _),
            buffer.len() as u32,
            Some(&mut bytes_returned),
            None,
        );

        let _ = CloseHandle(handle);

        if result.is_ok() {
            let extents = &*(buffer.as_ptr() as *const VolumeDiskExtents);
            if extents.number_of_disk_extents > 0 {
                return Some(extents.extents[0].starting_offset as u64);
            }
        }

        None
    }
}

/// 获取卷信息（卷标、文件系统、已用空间、空闲空间）
#[cfg(windows)]
fn get_volume_info(letter: char) -> (String, String, u64, u64) {
    use windows::Win32::Storage::FileSystem::{GetVolumeInformationW, GetDiskFreeSpaceExW};

    let path = format!("{}:\\", letter);
    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    let mut volume_name = [0u16; 261];
    let mut file_system_name = [0u16; 261];

    let (label, file_system) = unsafe {
        let result = GetVolumeInformationW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut volume_name),
            None,
            None,
            None,
            Some(&mut file_system_name),
        );

        if result.is_ok() {
            let label = String::from_utf16_lossy(&volume_name)
                .trim_end_matches('\0')
                .to_string();
            let file_system = String::from_utf16_lossy(&file_system_name)
                .trim_end_matches('\0')
                .to_string();
            (label, file_system)
        } else {
            (String::new(), String::new())
        }
    };

    // 获取磁盘空间信息
    let (used_bytes, free_bytes) = unsafe {
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        let result = GetDiskFreeSpaceExW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut free_bytes_available as *mut u64),
            Some(&mut total_bytes as *mut u64),
            Some(&mut total_free_bytes as *mut u64),
        );

        if result.is_ok() && total_bytes > 0 {
            let used = total_bytes.saturating_sub(total_free_bytes);
            (used, total_free_bytes)
        } else {
            (0, 0)
        }
    };

    (label, file_system, used_bytes, free_bytes)
}

/// 获取磁盘型号
#[cfg(windows)]
fn get_disk_model(disk_number: u32) -> Option<String> {
    use crate::utils::cmd::create_command;

    // 使用 PowerShell 获取磁盘型号
    let output = create_command("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Get-Disk -Number {} | Select-Object -ExpandProperty FriendlyName",
                disk_number
            ),
        ])
        .output()
        .ok()?;

    if output.status.success() {
        let model = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !model.is_empty() {
            return Some(model);
        }
    }

    // 备选：使用 WMIC
    let output = create_command("wmic")
        .args([
            "diskdrive",
            "where",
            &format!("Index={}", disk_number),
            "get",
            "Model",
            "/format:list",
        ])
        .output()
        .ok()?;

    let text = gbk_to_utf8(&output.stdout);
    for line in text.lines() {
        if line.starts_with("Model=") {
            let model = line.trim_start_matches("Model=").trim().to_string();
            if !model.is_empty() {
                return Some(model);
            }
        }
    }

    None
}

/// 执行一键分区操作
pub fn execute_quick_partition(
    disk_number: u32,
    partition_style: PartitionStyle,
    layouts: &[PartitionLayout],
) -> QuickPartitionResult {
    log::info!(
        "开始一键分区: 磁盘 {}, 分区表类型: {:?}, 分区数量: {}",
        disk_number,
        partition_style,
        layouts.len()
    );

    // 构建 diskpart 脚本
    let mut script = String::new();

    // 选择磁盘
    script.push_str(&format!("select disk {}\n", disk_number));

    // 清除磁盘（删除所有分区）
    script.push_str("clean\n");

    // 转换分区表类型
    match partition_style {
        PartitionStyle::GPT => {
            script.push_str("convert gpt\n");
        }
        PartitionStyle::MBR => {
            script.push_str("convert mbr\n");
        }
        _ => {
            return QuickPartitionResult {
                success: false,
                message: "无效的分区表类型".to_string(),
                created_partitions: Vec::new(),
            };
        }
    }

    let mut created_partitions = Vec::new();

    // 创建分区
    for (i, layout) in layouts.iter().enumerate() {
        let is_last = i == layouts.len() - 1;

        if layout.is_esp {
            // 创建 ESP 分区
            let size_mb = (layout.size_gb * 1024.0) as u64;
            script.push_str(&format!("create partition efi size={}\n", size_mb));
            script.push_str("format fs=fat32 quick label=\"EFI\"\n");
            created_partitions.push("ESP".to_string());
        } else {
            // 创建普通分区
            if is_last {
                // 最后一个分区使用剩余空间
                script.push_str("create partition primary\n");
            } else {
                let size_mb = (layout.size_gb * 1024.0) as u64;
                script.push_str(&format!("create partition primary size={}\n", size_mb));
            }

            // 格式化
            let label = if layout.label.is_empty() {
                "新加卷".to_string()
            } else {
                layout.label.clone()
            };
            let fs = if layout.file_system.is_empty() {
                "NTFS"
            } else {
                &layout.file_system
            };
            script.push_str(&format!("format fs={} quick label=\"{}\"\n", fs, label));

            // 分配盘符
            if let Some(letter) = layout.drive_letter {
                script.push_str(&format!("assign letter={}\n", letter));
                created_partitions.push(format!("{}:", letter));
            } else {
                script.push_str("assign\n");
                created_partitions.push(format!("分区 {}", i + 1));
            }
        }
    }

    // 执行脚本
    match execute_diskpart_script(&script) {
        Ok(output) => {
            // 检查输出是否包含错误
            let output_lower = output.to_lowercase();
            if output_lower.contains("错误")
                || output_lower.contains("error")
                || output_lower.contains("失败")
                || output_lower.contains("failed")
            {
                QuickPartitionResult {
                    success: false,
                    message: format!("分区操作失败: {}", output),
                    created_partitions: Vec::new(),
                }
            } else {
                QuickPartitionResult {
                    success: true,
                    message: "分区操作完成".to_string(),
                    created_partitions,
                }
            }
        }
        Err(e) => QuickPartitionResult {
            success: false,
            message: format!("执行 diskpart 失败: {}", e),
            created_partitions: Vec::new(),
        },
    }
}

/// 执行 diskpart 脚本
fn execute_diskpart_script(script: &str) -> Result<String> {
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join("lr_quick_partition.txt");

    log::debug!("Diskpart 脚本内容:\n{}", script);

    std::fs::write(&script_path, script)?;

    let output = create_command(&get_diskpart_path())
        .args(["/s", script_path.to_str().unwrap()])
        .output()?;

    let _ = std::fs::remove_file(&script_path);

    let output_text = gbk_to_utf8(&output.stdout);
    let error_text = gbk_to_utf8(&output.stderr);

    log::info!("Diskpart 输出: {}", output_text);
    if !error_text.is_empty() {
        log::warn!("Diskpart 错误: {}", error_text);
    }

    if !error_text.is_empty() && !output.status.success() {
        anyhow::bail!("{}", error_text);
    }

    Ok(output_text)
}

/// 检查磁盘是否可以安全分区（没有系统盘）
pub fn can_safely_partition(disk: &PhysicalDisk) -> (bool, String) {
    // 检查是否包含系统盘
    let system_drive = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".to_string())
        .chars()
        .next()
        .unwrap_or('C');

    for partition in &disk.partitions {
        if let Some(letter) = partition.drive_letter {
            if letter == system_drive {
                return (
                    false,
                    format!(
                        "磁盘 {} 包含当前系统盘 {}:，无法进行一键分区",
                        disk.disk_number, system_drive
                    ),
                );
            }
        }
    }

    // 检查是否有 Windows 系统
    for partition in &disk.partitions {
        if let Some(letter) = partition.drive_letter {
            let windows_path = format!("{}:\\Windows\\System32", letter);
            if Path::new(&windows_path).exists() {
                return (
                    false,
                    format!(
                        "磁盘 {} 上的分区 {}: 包含 Windows 系统，请先备份数据",
                        disk.disk_number, letter
                    ),
                );
            }
        }
    }

    (true, String::new())
}

/// 根据启动模式获取推荐的分区表类型
pub fn get_recommended_partition_style(boot_mode: &BootMode) -> PartitionStyle {
    match boot_mode {
        BootMode::UEFI => PartitionStyle::GPT,
        BootMode::Legacy => PartitionStyle::MBR,
    }
}

/// 获取下一个可用的盘符
pub fn get_next_available_drive_letter(used_letters: &[char]) -> Option<char> {
    for letter in 'C'..='Z' {
        if !used_letters.contains(&letter) && !used_letters.contains(&letter.to_ascii_lowercase()) {
            // 检查盘符是否已被系统使用
            let path = format!("{}:\\", letter);
            if !Path::new(&path).exists() {
                return Some(letter);
            }
        }
    }
    None
}

/// 获取所有已使用的盘符
pub fn get_used_drive_letters() -> Vec<char> {
    let mut letters = Vec::new();
    for letter in 'A'..='Z' {
        let path = format!("{}:\\", letter);
        if Path::new(&path).exists() {
            letters.push(letter);
        }
    }
    letters
}

/// 创建单个分区
pub fn create_single_partition(
    disk_number: u32,
    size_mb: u64,
    drive_letter: Option<char>,
    label: &str,
) -> Result<String> {
    let mut script = String::new();

    script.push_str(&format!("select disk {}\n", disk_number));
    
    if size_mb > 0 {
        script.push_str(&format!("create partition primary size={}\n", size_mb));
    } else {
        // 使用所有剩余空间
        script.push_str("create partition primary\n");
    }

    let vol_label = if label.is_empty() { "OS" } else { label };
    script.push_str(&format!("format fs=ntfs quick label=\"{}\"\n", vol_label));

    if let Some(letter) = drive_letter {
        script.push_str(&format!("assign letter={}\n", letter));
    } else {
        script.push_str("assign\n");
    }

    execute_diskpart_script(&script)
}

/// 创建 ESP 分区
pub fn create_esp_partition(disk_number: u32, size_mb: u64) -> Result<String> {
    let mut script = String::new();

    script.push_str(&format!("select disk {}\n", disk_number));
    script.push_str(&format!("create partition efi size={}\n", size_mb));
    script.push_str("format fs=fat32 quick label=\"EFI\"\n");

    execute_diskpart_script(&script)
}

/// 删除指定分区
pub fn delete_partition(disk_number: u32, partition_number: u32) -> Result<String> {
    let mut script = String::new();

    script.push_str(&format!("select disk {}\n", disk_number));
    script.push_str(&format!("select partition {}\n", partition_number));
    script.push_str("delete partition override\n");

    execute_diskpart_script(&script)
}

/// 缩小分区
pub fn shrink_partition(disk_number: u32, partition_number: u32, shrink_mb: u64) -> Result<String> {
    let mut script = String::new();

    script.push_str(&format!("select disk {}\n", disk_number));
    script.push_str(&format!("select partition {}\n", partition_number));
    script.push_str(&format!("shrink desired={}\n", shrink_mb));

    execute_diskpart_script(&script)
}

/// 扩展分区
pub fn extend_partition(
    disk_number: u32,
    partition_number: u32,
    extend_mb: Option<u64>,
) -> Result<String> {
    let mut script = String::new();

    script.push_str(&format!("select disk {}\n", disk_number));
    script.push_str(&format!("select partition {}\n", partition_number));

    if let Some(size) = extend_mb {
        script.push_str(&format!("extend size={}\n", size));
    } else {
        // 使用所有可用空间
        script.push_str("extend\n");
    }

    execute_diskpart_script(&script)
}

/// 调整已有分区大小的结果
#[derive(Debug, Clone)]
pub struct ResizePartitionResult {
    pub success: bool,
    pub message: String,
    pub new_size_mb: u64,
}

/// 调整已有分区大小
///
/// # 参数
/// - `disk_number`: 磁盘编号
/// - `partition_number`: 分区编号
/// - `drive_letter`: 分区盘符（用于获取空间信息）
/// - `current_size_mb`: 当前分区大小（MB）
/// - `new_size_mb`: 目标大小（MB）
/// - `used_mb`: 已使用空间（MB）
///
/// # 返回
/// - `ResizePartitionResult`: 包含操作结果和新大小
pub fn resize_existing_partition(
    disk_number: u32,
    partition_number: u32,
    drive_letter: Option<char>,
    current_size_mb: u64,
    new_size_mb: u64,
    used_mb: u64,
) -> ResizePartitionResult {
    log::info!(
        "调整分区大小: 磁盘 {} 分区 {}, 当前 {} MB, 目标 {} MB, 已用 {} MB",
        disk_number,
        partition_number,
        current_size_mb,
        new_size_mb,
        used_mb
    );

    // 验证：新大小必须大于已使用空间（留100MB余量）
    let min_size_mb = used_mb + 100;
    if new_size_mb < min_size_mb {
        return ResizePartitionResult {
            success: false,
            message: format!(
                "目标大小 {} MB 必须大于已使用空间 {} MB (最小 {} MB)",
                new_size_mb, used_mb, min_size_mb
            ),
            new_size_mb: current_size_mb,
        };
    }

    // 验证：新大小必须大于0
    if new_size_mb == 0 {
        return ResizePartitionResult {
            success: false,
            message: "目标大小不能为0".to_string(),
            new_size_mb: current_size_mb,
        };
    }

    // 检查是否需要调整
    if new_size_mb == current_size_mb {
        return ResizePartitionResult {
            success: true,
            message: "分区大小未改变".to_string(),
            new_size_mb: current_size_mb,
        };
    }

    // 判断是缩小还是扩大
    if new_size_mb < current_size_mb {
        // 缩小分区
        let shrink_amount_mb = current_size_mb - new_size_mb;
        
        log::info!("缩小分区 {} MB", shrink_amount_mb);

        // 使用 diskpart shrink 命令
        // 注意：diskpart 的 shrink 命令需要通过卷来选择，而不是分区
        let result = if let Some(letter) = drive_letter {
            // 通过盘符选择卷进行缩小
            let mut script = String::new();
            script.push_str(&format!("select volume {}\n", letter));
            script.push_str(&format!("shrink desired={} minimum={}\n", shrink_amount_mb, shrink_amount_mb));
            execute_diskpart_script(&script)
        } else {
            // 没有盘符的分区使用分区编号
            let mut script = String::new();
            script.push_str(&format!("select disk {}\n", disk_number));
            script.push_str(&format!("select partition {}\n", partition_number));
            script.push_str(&format!("shrink desired={} minimum={}\n", shrink_amount_mb, shrink_amount_mb));
            execute_diskpart_script(&script)
        };

        match result {
            Ok(output) => {
                let output_lower = output.to_lowercase();
                if output_lower.contains("错误")
                    || output_lower.contains("error")
                    || output_lower.contains("失败")
                    || output_lower.contains("failed")
                    || output_lower.contains("没有足够")
                    || output_lower.contains("insufficient")
                {
                    ResizePartitionResult {
                        success: false,
                        message: format!("缩小分区失败: {}", output.trim()),
                        new_size_mb: current_size_mb,
                    }
                } else {
                    ResizePartitionResult {
                        success: true,
                        message: format!("分区已成功缩小 {} MB", shrink_amount_mb),
                        new_size_mb,
                    }
                }
            }
            Err(e) => ResizePartitionResult {
                success: false,
                message: format!("执行 diskpart 失败: {}", e),
                new_size_mb: current_size_mb,
            },
        }
    } else {
        // 扩大分区
        let extend_amount_mb = new_size_mb - current_size_mb;
        
        log::info!("扩大分区 {} MB", extend_amount_mb);

        // 使用 diskpart extend 命令
        let result = if let Some(letter) = drive_letter {
            // 通过盘符选择卷进行扩展
            let mut script = String::new();
            script.push_str(&format!("select volume {}\n", letter));
            script.push_str(&format!("extend size={}\n", extend_amount_mb));
            execute_diskpart_script(&script)
        } else {
            // 没有盘符的分区使用分区编号
            let mut script = String::new();
            script.push_str(&format!("select disk {}\n", disk_number));
            script.push_str(&format!("select partition {}\n", partition_number));
            script.push_str(&format!("extend size={}\n", extend_amount_mb));
            execute_diskpart_script(&script)
        };

        match result {
            Ok(output) => {
                let output_lower = output.to_lowercase();
                if output_lower.contains("错误")
                    || output_lower.contains("error")
                    || output_lower.contains("失败")
                    || output_lower.contains("failed")
                    || output_lower.contains("没有足够")
                    || output_lower.contains("insufficient")
                {
                    ResizePartitionResult {
                        success: false,
                        message: format!("扩展分区失败: {}", output.trim()),
                        new_size_mb: current_size_mb,
                    }
                } else {
                    ResizePartitionResult {
                        success: true,
                        message: format!("分区已成功扩展 {} MB", extend_amount_mb),
                        new_size_mb,
                    }
                }
            }
            Err(e) => ResizePartitionResult {
                success: false,
                message: format!("执行 diskpart 失败: {}", e),
                new_size_mb: current_size_mb,
            },
        }
    }
}

/// 查询分区可缩小的最大空间（MB）
/// 
/// 使用 diskpart 的 shrink querymax 命令获取
pub fn query_shrink_max(drive_letter: char) -> Result<u64> {
    let mut script = String::new();
    script.push_str(&format!("select volume {}\n", drive_letter));
    script.push_str("shrink querymax\n");

    let output = execute_diskpart_script(&script)?;
    
    // 解析输出，查找可缩小的最大值
    // 输出格式通常为 "可回收的最大字节数:  XXX MB" 或 "The maximum number of reclaimable bytes is: XXX MB"
    for line in output.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("可回收") || line_lower.contains("reclaimable") || line_lower.contains("maximum") {
            // 尝试提取数字
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, part) in parts.iter().enumerate() {
                if let Ok(num) = part.replace(",", "").replace(".", "").parse::<u64>() {
                    // 检查下一个部分是否是 MB 或 GB
                    if i + 1 < parts.len() {
                        let unit = parts[i + 1].to_uppercase();
                        if unit.starts_with("MB") || unit.starts_with("M") {
                            return Ok(num);
                        } else if unit.starts_with("GB") || unit.starts_with("G") {
                            return Ok(num * 1024);
                        }
                    }
                    // 如果没有单位，假设是 MB
                    return Ok(num);
                }
            }
        }
    }

    // 如果无法解析，返回0
    log::warn!("无法解析 shrink querymax 输出: {}", output);
    Ok(0)
}

/// 获取磁盘上指定分区后面的未分配空间大小（MB）
/// 
/// 这用于判断分区是否可以扩展
/// 注意：此函数需要传入已有的磁盘信息，避免重复获取
pub fn get_unallocated_space_after_partition_with_disk(disk: &PhysicalDisk, partition_number: u32) -> u64 {
    // 找到目标分区
    let target_partition = match disk.partitions.iter().find(|p| p.partition_number == partition_number) {
        Some(p) => p,
        None => return 0,
    };

    // 计算该分区的结束位置
    let partition_end = target_partition.offset_bytes + target_partition.size_bytes;

    // 找到紧邻的下一个分区
    // 注意：使用 >= 而不是 >，因为如果分区紧邻（offset == partition_end），
    // 则没有未分配空间，next_start - partition_end = 0
    let mut next_partition_start: Option<u64> = None;
    for p in &disk.partitions {
        if p.offset_bytes >= partition_end && p.partition_number != partition_number {
            match next_partition_start {
                None => next_partition_start = Some(p.offset_bytes),
                Some(current) => {
                    if p.offset_bytes < current {
                        next_partition_start = Some(p.offset_bytes);
                    }
                }
            }
        }
    }

    // 计算未分配空间
    let unallocated = match next_partition_start {
        Some(next_start) => next_start.saturating_sub(partition_end),
        None => disk.size_bytes.saturating_sub(partition_end),
    };

    // 转换为 MB
    unallocated / 1024 / 1024
}

/// 获取磁盘上指定分区后面的未分配空间大小（MB）
/// 
/// 兼容旧API，内部会获取磁盘信息（较慢）
pub fn get_unallocated_space_after_partition(disk_number: u32, partition_number: u32) -> u64 {
    let disks = get_physical_disks();
    match disks.iter().find(|d| d.disk_number == disk_number) {
        Some(disk) => get_unallocated_space_after_partition_with_disk(disk, partition_number),
        None => 0,
    }
}

/// 检查分区是否可以调整大小
/// 
/// 返回 (是否可调整, 原因说明, 最小大小MB, 最大大小MB)
pub fn can_resize_partition(partition: &DiskPartitionInfo, disk: &PhysicalDisk) -> (bool, String, u64, u64) {
    // 检查是否是特殊分区
    if partition.is_esp {
        return (false, "ESP分区不支持调整大小".to_string(), 0, 0);
    }
    if partition.is_msr {
        return (false, "MSR分区不支持调整大小".to_string(), 0, 0);
    }
    if partition.is_recovery {
        return (false, "恢复分区不支持调整大小".to_string(), 0, 0);
    }

    // 检查是否有盘符（没有盘符的分区可能无法正常操作）
    if partition.drive_letter.is_none() {
        return (false, "分区没有盘符，无法调整大小".to_string(), 0, 0);
    }

    let drive_letter = partition.drive_letter.unwrap();

    // 检查是否是当前系统盘
    let system_drive = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".to_string())
        .chars()
        .next()
        .unwrap_or('C');

    if drive_letter == system_drive {
        return (false, "无法调整当前系统分区大小".to_string(), 0, 0);
    }

    // 计算最小大小（已使用空间 + 100MB 余量）
    let used_mb = partition.used_bytes / 1024 / 1024;
    let min_size_mb = used_mb + 100;

    // 计算最大大小
    let current_size_mb = partition.size_bytes / 1024 / 1024;
    let unallocated_after_mb = get_unallocated_space_after_partition(
        disk.disk_number,
        partition.partition_number,
    );
    let max_size_mb = current_size_mb + unallocated_after_mb;

    // 如果没有可调整的空间
    if min_size_mb >= max_size_mb {
        return (
            false,
            format!(
                "分区无法调整大小，已用空间 {} MB 接近分区大小 {} MB",
                used_mb, current_size_mb
            ),
            0,
            0,
        );
    }

    (
        true,
        format!(
            "可调整范围: {} MB - {} MB (已用: {} MB)",
            min_size_mb, max_size_mb, used_mb
        ),
        min_size_mb,
        max_size_mb,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_used_drive_letters() {
        let letters = get_used_drive_letters();
        // C: 应该总是存在
        assert!(letters.contains(&'C'));
    }

    #[test]
    fn test_get_next_available_drive_letter() {
        let used = vec!['C', 'D', 'E'];
        let next = get_next_available_drive_letter(&used);
        assert!(next.is_some());
        assert!(!used.contains(&next.unwrap()));
    }
}
