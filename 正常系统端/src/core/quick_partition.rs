//! 一键分区核心模块
//!
//! 提供磁盘分区的底层操作功能，所有查询和写入均使用文档化 Windows API。

use anyhow::{anyhow, Result};
use std::path::Path;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
        IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_STYLE_GPT,
        PARTITION_STYLE_MBR, PARTITION_STYLE_RAW, STORAGE_DEVICE_DESCRIPTOR,
        STORAGE_PROPERTY_QUERY,
    },
    Win32::System::IO::DeviceIoControl,
};

/// IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS 常量
/// CTL_CODE(IOCTL_VOLUME_BASE, 0, METHOD_BUFFERED, FILE_ANY_ACCESS)
/// IOCTL_VOLUME_BASE = 0x56 ('V'), 所以值为 (0x56 << 16) | (0 << 14) | (0 << 2) | 0 = 0x00560000
#[cfg(windows)]
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x00560000;

use crate::tr;

use super::disk::PartitionStyle;
use super::system_info::BootMode;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

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
            tr!(
                "磁盘 {} ({} GB)",
                self.disk_number,
                format!("{:.1}", self.size_gb())
            )
        } else {
            tr!(
                "磁盘 {} - {} ({} GB)",
                self.disk_number,
                self.model,
                format!("{:.1}", self.size_gb())
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
    /// 是否为活动分区（MBR BootIndicator=0x80；权威来源，直接读 MBR 引导字节，GPT 恒为 false）
    pub is_active: bool,
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
            tr!("恢复分区")
        } else {
            tr!("分区 {}", self.partition_number)
        }
    }
}

/// 用户设计的分区布局
#[derive(Debug, Clone, PartialEq)]
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
    geometry_cylinders: i64,  // 8 bytes - LARGE_INTEGER 必须在最前面！
    geometry_media_type: u32, // 4 bytes
    geometry_tracks_per_cylinder: u32, // 4 bytes
    geometry_sectors_per_track: u32, // 4 bytes
    geometry_bytes_per_sector: u32, // 4 bytes
    // DISK_GEOMETRY_EX 扩展部分
    disk_size: i64, // 8 bytes - 这才是我们需要的磁盘大小
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

/// 返回指定磁盘上活动（引导）分区的分区号（MBR BootIndicator=0x80）。
///
/// 权威来源：经 IOCTL_DISK_GET_DRIVE_LAYOUT_EX 直接读 MBR 引导字节，不依赖 diskpart 文本输出
/// （新版 Windows 的 `detail partition` 可能不显示"活动"字段，`list partition` 的 `*` 只是焦点标记）。
/// 无活动分区或非 MBR 盘返回 None。
#[cfg(windows)]
pub fn get_active_partition_number(disk_number: u32) -> Option<u32> {
    let disk = get_disk_info(disk_number)?;
    disk.partitions
        .iter()
        .find(|p| p.is_active)
        .map(|p| p.partition_number)
}

#[cfg(not(windows))]
pub fn get_active_partition_number(_disk_number: u32) -> Option<u32> {
    None
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
            let partitions = parse_partition_layout(&buffer, header, style, disk_number);

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
    disk_number: u32,
) -> Vec<DiskPartitionInfo> {
    let mut partitions = Vec::new();

    // PARTITION_INFORMATION_EX 结构大小固定为 144 字节
    let partition_entry_size = 144;

    // DRIVE_LAYOUT_INFORMATION_EX 头部大小 = FIELD_OFFSET(_, PartitionEntry)：
    //   PartitionStyle(4) + PartitionCount(4) + union{Mbr,Gpt}(40) = 48。
    // 关键：那个 union 是【定长】的——大小取最大成员 max(MBR=8, GPT=40)=40、按 8 对齐，
    // 与磁盘实际是 MBR 还是 GPT 无关。所以分区数组对 MBR 和 GPT 都从偏移 48 起。
    // （旧代码对 MBR 用 16 是把 union 当成只占 8 字节，导致 MBR 盘每个分区项整体前移 32 字节、
    //   字段全部错位——partition_length 落进 union 区读成 ~0 被跳过，MBR 盘解析出 0 个分区，
    //   进而 is_active/盘符匹配全失效，Legacy 引导回退、开机 0x7B。GPT 一直用 48 故正常。）
    let header_size = 8 + 40; // = 48，MBR/GPT 一致

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

        let starting_offset =
            i64::from_le_bytes(partition_data[8..16].try_into().unwrap_or([0; 8]));
        let partition_length =
            i64::from_le_bytes(partition_data[16..24].try_into().unwrap_or([0; 8]));
        let partition_number =
            u32::from_le_bytes(partition_data[24..28].try_into().unwrap_or([0; 4]));

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

        // 活动分区标志（仅 MBR 有意义）：BootIndicator(union 偏移 1 -> partition_data[33]) 高位置位即活动分区。
        // 直接读 MBR 引导字节，是判断活动分区的权威来源——diskpart `detail partition` 在新版 Windows
        // 上可能根本不显示"活动"字段，`list partition` 的 `*` 又只表示焦点而非活动，都不可靠。
        let is_active = style == PartitionStyle::MBR && (partition_data[33] & 0x80) != 0;

        // 获取盘符（按【磁盘号 + 偏移】匹配，避免多盘机器上两盘同偏移分区被错配同一盘符）
        let drive_letter = get_drive_letter_for_partition(disk_number, starting_offset as u64);

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
            is_active,
        });
    }

    // 按偏移量排序
    partitions.sort_by_key(|p| p.offset_bytes);

    partitions
}

/// 根据【磁盘号 + 分区偏移量】获取对应的盘符。
/// 必须同时比对磁盘号：多盘机器上两块盘的首分区往往都在 1MiB，仅比偏移会把一块盘的分区
/// 错配成另一块盘上同偏移卷的盘符，进而让上层（引导分区定位）在错误的磁盘上写引导/设活动。
#[cfg(windows)]
fn get_drive_letter_for_partition(disk_number: u32, offset: u64) -> Option<char> {
    for letter in b'C'..=b'Z' {
        let c = letter as char;
        let path = format!("{}:\\", c);
        if !Path::new(&path).exists() {
            continue;
        }

        // 检查这个卷的磁盘号与偏移量是否都匹配
        if let Some((vol_disk, vol_offset)) = get_volume_offset(c) {
            if vol_disk == disk_number
                && (vol_offset as i64 - offset as i64).unsigned_abs() < 1024 * 1024
            {
                return Some(c);
            }
        }
    }
    None
}

/// 获取卷所在的【磁盘号 + 起始偏移量】（DISK_EXTENT.DiskNumber / StartingOffset）。
/// DiskNumber 即 \\.\PhysicalDriveN 的 N，与 get_disk_info 用的磁盘号同义。
#[cfg(windows)]
fn get_volume_offset(letter: char) -> Option<(u32, u64)> {
    unsafe {
        let volume_path = format!("\\\\.\\{}:", letter);
        let wide_path: Vec<u16> = volume_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

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
                let e = &extents.extents[0];
                return Some((e.disk_number, e.starting_offset as u64));
            }
        }

        None
    }
}

/// 获取卷信息（卷标、文件系统、已用空间、空闲空间）
#[cfg(windows)]
fn get_volume_info(letter: char) -> (String, String, u64, u64) {
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW};

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
    unsafe {
        let path = format!(r"\\.\PhysicalDrive{disk_number}");
        let wide = path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .ok()?;
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        // u64 storage gives the descriptor its required native alignment.
        let mut storage = vec![0_u64; 512];
        let mut returned = 0_u32;
        let result = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(storage.as_mut_ptr().cast()),
            (storage.len() * std::mem::size_of::<u64>()) as u32,
            Some(&mut returned),
            None,
        );
        let _ = CloseHandle(handle);
        if result.is_err() || returned < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), returned as usize);
        let descriptor = &*bytes.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>();
        let vendor = descriptor_string(bytes, descriptor.VendorIdOffset);
        let product = descriptor_string(bytes, descriptor.ProductIdOffset);
        let model = match (vendor, product) {
            (Some(vendor), Some(product)) if !product.starts_with(&vendor) => {
                format!("{vendor} {product}")
            }
            (_, Some(product)) => product,
            (Some(vendor), None) => vendor,
            (None, None) => return None,
        };
        (!model.is_empty()).then_some(model)
    }
}

fn descriptor_string(buffer: &[u8], offset: u32) -> Option<String> {
    let offset = usize::try_from(offset).ok()?;
    if offset == 0 || offset >= buffer.len() {
        return None;
    }
    let end = buffer[offset..].iter().position(|byte| *byte == 0)?;
    let value = String::from_utf8_lossy(&buffer[offset..offset + end])
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
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

    let disks = get_physical_disks();

    let Some(disk) = disks.iter().find(|d| d.disk_number == disk_number) else {
        return QuickPartitionResult {
            success: false,
            message: tr!("磁盘 {} 不存在，已取消一键分区", disk_number),
            created_partitions: Vec::new(),
        };
    };

    let (safe, reason) = can_safely_partition(disk);
    if !safe {
        return QuickPartitionResult {
            success: false,
            message: reason,
            created_partitions: Vec::new(),
        };
    }

    execute_quick_partition_validated(disk_number, partition_style, layouts)
}

/// Executes a plan whose physical-disk identity and current safety state were
/// already revalidated by the caller immediately before this boundary.
pub(crate) fn execute_quick_partition_validated(
    disk_number: u32,
    partition_style: PartitionStyle,
    layouts: &[PartitionLayout],
) -> QuickPartitionResult {
    let style = match storage_style(partition_style) {
        Ok(style) => style,
        Err(error) => return quick_partition_failure(error),
    };
    if layouts.is_empty() {
        return quick_partition_failure(anyhow!("分区方案不能为空"));
    }
    if let Err(error) = lr_core::windows_storage::clean_and_initialize(disk_number, style) {
        return quick_partition_failure(anyhow!("清除并初始化磁盘失败: {error}"));
    }

    let mut expected = Vec::with_capacity(layouts.len());
    let mut created_partitions = Vec::new();
    let mut assigned_letters = get_used_drive_letters();
    for (i, layout) in layouts.iter().enumerate() {
        let is_last = i == layouts.len() - 1;
        let file_system = match storage_file_system(&layout.file_system) {
            Ok(value) => value,
            Err(error) => return quick_partition_failure(error),
        };
        let size_bytes = if is_last {
            0
        } else {
            match gib_to_bytes(layout.size_gb) {
                Ok(value) => value,
                Err(error) => return quick_partition_failure(error),
            }
        };
        let label = if layout.is_esp {
            "EFI".to_string()
        } else if layout.label.is_empty() {
            tr!("新加卷")
        } else {
            layout.label.clone()
        };
        let drive_letter = if layout.is_esp {
            None
        } else if let Some(letter) = layout.drive_letter {
            Some(letter.to_ascii_uppercase())
        } else {
            get_next_available_drive_letter(&assigned_letters)
        };
        if let Some(letter) = drive_letter {
            assigned_letters.push(letter);
        }
        let request = lr_core::windows_storage::CreatePartitionRequest {
            disk_number,
            offset_bytes: 0,
            size_bytes,
            kind: if layout.is_esp {
                lr_core::windows_storage::PartitionKind::EfiSystem
            } else {
                lr_core::windows_storage::PartitionKind::BasicData
            },
            file_system: Some(file_system),
            label,
            drive_letter,
            active: false,
            preserve_gpt_metadata: None,
        };
        match lr_core::windows_storage::create_partition(&request) {
            Ok(created) => {
                expected.push(created);
                created_partitions.push(
                    drive_letter
                        .map(|letter| format!("{letter}:"))
                        .unwrap_or_else(|| {
                            if layout.is_esp {
                                "ESP".to_string()
                            } else {
                                tr!("分区 {}", i + 1)
                            }
                        }),
                );
            }
            Err(error) => {
                return quick_partition_failure(anyhow!(
                    "创建第 {} 个分区失败；磁盘当前可能处于部分完成状态，请刷新后检查: {}",
                    i + 1,
                    error
                ));
            }
        }
    }

    let Some(current) = get_physical_disks()
        .into_iter()
        .find(|disk| disk.disk_number == disk_number)
    else {
        return quick_partition_failure(anyhow!("操作后无法重新读取目标磁盘"));
    };
    if current.partition_style != partition_style {
        return quick_partition_failure(anyhow!("操作后的分区表类型与请求不一致"));
    }
    for created in &expected {
        let verified = current.partitions.iter().any(|partition| {
            partition.offset_bytes == created.offset_bytes
                && partition.size_bytes == created.size_bytes
        });
        if !verified {
            return quick_partition_failure(anyhow!(
                "操作后核验失败：偏移 {}、大小 {} 字节的分区不存在",
                created.offset_bytes,
                created.size_bytes
            ));
        }
    }
    QuickPartitionResult {
        success: true,
        message: tr!("分区操作完成"),
        created_partitions,
    }
}

fn quick_partition_failure(error: anyhow::Error) -> QuickPartitionResult {
    log::error!("一键分区失败: {error:#}");
    QuickPartitionResult {
        success: false,
        message: tr!("分区操作失败: {}", error),
        created_partitions: Vec::new(),
    }
}

fn storage_style(style: PartitionStyle) -> Result<lr_core::windows_storage::DiskStyle> {
    match style {
        PartitionStyle::GPT => Ok(lr_core::windows_storage::DiskStyle::Gpt),
        PartitionStyle::MBR => Ok(lr_core::windows_storage::DiskStyle::Mbr),
        _ => Err(anyhow!("无效的分区表类型")),
    }
}

fn storage_file_system(value: &str) -> Result<lr_core::windows_storage::FileSystem> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("NTFS") {
        Ok(lr_core::windows_storage::FileSystem::Ntfs)
    } else if value.eq_ignore_ascii_case("FAT32") {
        Ok(lr_core::windows_storage::FileSystem::Fat32)
    } else if value.eq_ignore_ascii_case("EXFAT") {
        Ok(lr_core::windows_storage::FileSystem::ExFat)
    } else {
        Err(anyhow!("不支持的文件系统: {value}"))
    }
}

fn gib_to_bytes(size_gb: f64) -> Result<u64> {
    if !size_gb.is_finite() || size_gb <= 0.0 {
        return Err(anyhow!("分区大小必须是大于 0 的有限数值"));
    }
    let bytes = size_gb * GIB as f64;
    if bytes > u64::MAX as f64 {
        return Err(anyhow!("分区大小超出支持范围"));
    }
    Ok((bytes.round() as u64 / MIB) * MIB)
}

/// 检查磁盘是否可以安全分区（没有系统盘）
pub fn can_safely_partition(disk: &PhysicalDisk) -> (bool, String) {
    let system_drive = match lr_core::windows_storage::current_windows_drive_letter() {
        Ok(letter) => letter,
        Err(error) => {
            return (
                false,
                tr!("无法确认当前运行的系统卷，已阻止磁盘写入: {}", error),
            );
        }
    };
    if get_volume_offset(system_drive)
        .is_some_and(|(disk_number, _)| disk_number == disk.disk_number)
    {
        return (
            false,
            tr!(
                "磁盘 {} 包含当前运行的系统卷 {}:，无法进行一键分区",
                disk.disk_number,
                system_drive
            ),
        );
    }

    for partition in &disk.partitions {
        if let Some(letter) = partition.drive_letter {
            if letter.eq_ignore_ascii_case(&system_drive) {
                return (
                    false,
                    tr!(
                        "磁盘 {} 包含当前系统盘 {}:，无法进行一键分区",
                        disk.disk_number,
                        system_drive
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
                    tr!(
                        "磁盘 {} 上的分区 {}: 包含 Windows 系统，请先备份数据",
                        disk.disk_number,
                        letter
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
    let vol_label = if label.is_empty() { "OS" } else { label };
    let letter = drive_letter
        .map(|letter| letter.to_ascii_uppercase())
        .or_else(|| get_next_available_drive_letter(&get_used_drive_letters()))
        .ok_or_else(|| anyhow!("没有可用盘符"))?;
    let created = lr_core::windows_storage::create_partition(
        &lr_core::windows_storage::CreatePartitionRequest {
            disk_number,
            offset_bytes: 0,
            size_bytes: size_mb.saturating_mul(MIB),
            kind: lr_core::windows_storage::PartitionKind::BasicData,
            file_system: Some(lr_core::windows_storage::FileSystem::Ntfs),
            label: vol_label.to_string(),
            drive_letter: Some(letter),
            active: false,
            preserve_gpt_metadata: None,
        },
    )?;
    verify_created_partition(
        disk_number,
        created.offset_bytes,
        created.size_bytes,
        Some(letter),
    )?;
    Ok(format!("{letter}:"))
}

/// 创建 ESP 分区
pub fn create_esp_partition(disk_number: u32, size_mb: u64) -> Result<String> {
    let created = lr_core::windows_storage::create_partition(
        &lr_core::windows_storage::CreatePartitionRequest {
            disk_number,
            offset_bytes: 0,
            size_bytes: size_mb.saturating_mul(MIB),
            kind: lr_core::windows_storage::PartitionKind::EfiSystem,
            file_system: Some(lr_core::windows_storage::FileSystem::Fat32),
            label: "EFI".to_string(),
            drive_letter: None,
            active: false,
            preserve_gpt_metadata: None,
        },
    )?;
    verify_created_partition(disk_number, created.offset_bytes, created.size_bytes, None)?;
    Ok("ESP".to_string())
}

/// 删除指定分区
pub fn delete_partition(disk_number: u32, partition_number: u32) -> Result<String> {
    let partition = current_partition(disk_number, partition_number)?;
    reject_running_system_partition(disk_number, &partition)?;
    lr_core::windows_storage::delete_partition(disk_number, partition.offset_bytes, true)?;
    let still_exists = get_physical_disks()
        .into_iter()
        .find(|disk| disk.disk_number == disk_number)
        .is_some_and(|disk| {
            disk.partitions
                .iter()
                .any(|value| value.offset_bytes == partition.offset_bytes)
        });
    if still_exists {
        return Err(anyhow!("删除操作返回成功，但目标分区仍然存在"));
    }
    Ok(tr!("分区已删除"))
}

/// 缩小分区
pub fn shrink_partition(disk_number: u32, partition_number: u32, shrink_mb: u64) -> Result<String> {
    let partition = current_partition(disk_number, partition_number)?;
    reject_running_system_partition(disk_number, &partition)?;
    let letter = partition
        .drive_letter
        .ok_or_else(|| anyhow!("目标分区没有盘符，无法安全缩小文件系统"))?;
    let requested = shrink_mb
        .checked_mul(MIB)
        .ok_or_else(|| anyhow!("缩小大小超出支持范围"))?;
    let reclaimed = lr_core::windows_storage::shrink_volume(letter, requested, requested)?;
    verify_partition_delta(
        disk_number,
        partition.offset_bytes,
        partition.size_bytes,
        reclaimed,
        false,
    )?;
    Ok(tr!("分区已成功缩小 {} MB", reclaimed / MIB))
}

/// 扩展分区
pub fn extend_partition(
    disk_number: u32,
    partition_number: u32,
    extend_mb: Option<u64>,
) -> Result<String> {
    let disk = get_physical_disks()
        .into_iter()
        .find(|disk| disk.disk_number == disk_number)
        .ok_or_else(|| anyhow!("磁盘 {} 不存在", disk_number))?;
    let partition = disk
        .partitions
        .iter()
        .find(|partition| partition.partition_number == partition_number)
        .cloned()
        .ok_or_else(|| anyhow!("分区 {} 不存在", partition_number))?;
    reject_running_system_partition(disk_number, &partition)?;
    let letter = partition
        .drive_letter
        .ok_or_else(|| anyhow!("目标分区没有盘符，无法安全扩展文件系统"))?;
    let available_mb = get_unallocated_space_after_partition_with_disk(&disk, partition_number);
    let requested_mb = extend_mb.unwrap_or(available_mb);
    if requested_mb == 0 || requested_mb > available_mb {
        return Err(anyhow!(
            "请求扩展 {} MB，但分区后方只有 {} MB 连续未分配空间",
            requested_mb,
            available_mb
        ));
    }
    let requested = requested_mb
        .checked_mul(MIB)
        .ok_or_else(|| anyhow!("扩展大小超出支持范围"))?;
    lr_core::windows_storage::extend_volume(letter, disk_number, requested)?;
    verify_partition_delta(
        disk_number,
        partition.offset_bytes,
        partition.size_bytes,
        requested,
        true,
    )?;
    Ok(tr!("分区已成功扩展 {} MB", requested_mb))
}

fn current_partition(disk_number: u32, partition_number: u32) -> Result<DiskPartitionInfo> {
    get_physical_disks()
        .into_iter()
        .find(|disk| disk.disk_number == disk_number)
        .ok_or_else(|| anyhow!("磁盘 {} 不存在", disk_number))?
        .partitions
        .into_iter()
        .find(|partition| partition.partition_number == partition_number)
        .ok_or_else(|| anyhow!("分区 {} 不存在", partition_number))
}

fn reject_running_system_partition(disk_number: u32, partition: &DiskPartitionInfo) -> Result<()> {
    let system_letter = lr_core::windows_storage::current_windows_drive_letter()
        .map_err(|error| anyhow!("无法确认当前运行的系统卷，已阻止写入: {error}"))?;
    let Some((system_disk, system_offset)) = get_volume_offset(system_letter) else {
        return Err(anyhow!("无法解析当前运行系统卷的物理磁盘，已阻止写入"));
    };
    if system_disk == disk_number && system_offset == partition.offset_bytes {
        return Err(anyhow!("不能修改当前运行的系统分区 {system_letter}:"));
    }
    Ok(())
}

fn verify_created_partition(
    disk_number: u32,
    offset_bytes: u64,
    size_bytes: u64,
    drive_letter: Option<char>,
) -> Result<()> {
    let partition = get_physical_disks()
        .into_iter()
        .find(|disk| disk.disk_number == disk_number)
        .and_then(|disk| {
            disk.partitions
                .into_iter()
                .find(|partition| partition.offset_bytes == offset_bytes)
        })
        .ok_or_else(|| anyhow!("创建操作返回成功，但重新枚举时找不到新分区"))?;
    if partition.size_bytes != size_bytes {
        return Err(anyhow!(
            "新分区大小核验失败：期望 {} 字节，实际 {} 字节",
            size_bytes,
            partition.size_bytes
        ));
    }
    if drive_letter.map(|letter| letter.to_ascii_uppercase())
        != partition
            .drive_letter
            .map(|letter| letter.to_ascii_uppercase())
    {
        return Err(anyhow!("新分区盘符核验失败"));
    }
    Ok(())
}

fn verify_partition_delta(
    disk_number: u32,
    offset_bytes: u64,
    previous_size: u64,
    delta: u64,
    extending: bool,
) -> Result<()> {
    let current = get_physical_disks()
        .into_iter()
        .find(|disk| disk.disk_number == disk_number)
        .and_then(|disk| {
            disk.partitions
                .into_iter()
                .find(|partition| partition.offset_bytes == offset_bytes)
        })
        .ok_or_else(|| anyhow!("调整操作后无法重新定位目标分区"))?;
    let expected = if extending {
        previous_size.checked_add(delta)
    } else {
        previous_size.checked_sub(delta)
    }
    .ok_or_else(|| anyhow!("分区大小计算溢出"))?;
    if current.size_bytes != expected {
        return Err(anyhow!(
            "调整操作返回成功，但分区大小核验失败：期望 {} 字节，实际 {} 字节",
            expected,
            current.size_bytes
        ));
    }
    Ok(())
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
            message: tr!(
                "目标大小 {} MB 必须大于已使用空间 {} MB (最小 {} MB)",
                new_size_mb,
                used_mb,
                min_size_mb
            ),
            new_size_mb: current_size_mb,
        };
    }

    // 验证：新大小必须大于0
    if new_size_mb == 0 {
        return ResizePartitionResult {
            success: false,
            message: tr!("目标大小不能为0"),
            new_size_mb: current_size_mb,
        };
    }

    // 检查是否需要调整
    if new_size_mb == current_size_mb {
        return ResizePartitionResult {
            success: true,
            message: tr!("分区大小未改变"),
            new_size_mb: current_size_mb,
        };
    }

    let Some(letter) = drive_letter else {
        return ResizePartitionResult {
            success: false,
            message: tr!("分区没有盘符，无法安全调整文件系统大小"),
            new_size_mb: current_size_mb,
        };
    };
    let fresh = match current_partition(disk_number, partition_number) {
        Ok(partition) => partition,
        Err(error) => {
            return ResizePartitionResult {
                success: false,
                message: error.to_string(),
                new_size_mb: current_size_mb,
            };
        }
    };
    if fresh.drive_letter.map(|letter| letter.to_ascii_uppercase())
        != Some(letter.to_ascii_uppercase())
        || fresh.size_bytes / MIB != current_size_mb
    {
        return ResizePartitionResult {
            success: false,
            message: tr!("目标分区在执行前已发生变化，请刷新后重试"),
            new_size_mb: current_size_mb,
        };
    }
    let result = if new_size_mb < current_size_mb {
        shrink_partition(disk_number, partition_number, current_size_mb - new_size_mb)
    } else {
        extend_partition(
            disk_number,
            partition_number,
            Some(new_size_mb - current_size_mb),
        )
    };
    match result {
        Ok(message) => ResizePartitionResult {
            success: true,
            message,
            new_size_mb,
        },
        Err(error) => ResizePartitionResult {
            success: false,
            message: error.to_string(),
            new_size_mb: current_size_mb,
        },
    }
}

/// 查询分区可缩小的最大空间（MB）
///
pub fn query_shrink_max(drive_letter: char) -> Result<u64> {
    Ok(lr_core::windows_storage::query_max_reclaimable_bytes(drive_letter)? / MIB)
}

/// 获取磁盘上指定分区后面的未分配空间大小（MB）
///
/// 这用于判断分区是否可以扩展
/// 注意：此函数需要传入已有的磁盘信息，避免重复获取
pub fn get_unallocated_space_after_partition_with_disk(
    disk: &PhysicalDisk,
    partition_number: u32,
) -> u64 {
    get_unallocated_space_after_partition_bytes_with_disk(disk, partition_number) / 1024 / 1024
}

/// Returns the exact byte extent immediately following a partition.
///
/// Resize planning keeps this byte precision until the final MiB conversion. Flooring the
/// partition and the gap independently can lose one MiB and make a divider dragged to the end of
/// a valid unallocated extent appear to snap back.
pub fn get_unallocated_space_after_partition_bytes_with_disk(
    disk: &PhysicalDisk,
    partition_number: u32,
) -> u64 {
    // 找到目标分区
    let target_partition = match disk
        .partitions
        .iter()
        .find(|p| p.partition_number == partition_number)
    {
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
    match next_partition_start {
        Some(next_start) => next_start.saturating_sub(partition_end),
        None => disk.size_bytes.saturating_sub(partition_end),
    }
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
pub fn can_resize_partition(
    partition: &DiskPartitionInfo,
    disk: &PhysicalDisk,
) -> (bool, String, u64, u64) {
    // 检查是否是特殊分区
    if partition.is_esp {
        return (false, tr!("ESP分区不支持调整大小"), 0, 0);
    }
    if partition.is_msr {
        return (false, tr!("MSR分区不支持调整大小"), 0, 0);
    }
    if partition.is_recovery {
        return (false, tr!("恢复分区不支持调整大小"), 0, 0);
    }

    // 检查是否有盘符（没有盘符的分区可能无法正常操作）
    if partition.drive_letter.is_none() {
        return (false, tr!("分区没有盘符，无法调整大小"), 0, 0);
    }

    let drive_letter = partition.drive_letter.unwrap();

    let system_drive = match lr_core::windows_storage::current_windows_drive_letter() {
        Ok(letter) => letter,
        Err(error) => {
            return (
                false,
                tr!("无法确认当前运行的系统卷，已阻止调整大小: {}", error),
                0,
                0,
            );
        }
    };
    if get_volume_offset(system_drive).is_some_and(|(number, offset)| {
        number == disk.disk_number && offset == partition.offset_bytes
    }) || drive_letter.eq_ignore_ascii_case(&system_drive)
    {
        return (false, tr!("无法调整当前系统分区大小"), 0, 0);
    }

    // 计算最小大小（已使用空间 + 100MB 余量）
    let used_mb = partition.used_bytes / 1024 / 1024;
    let min_size_mb = used_mb + 100;

    // 计算最大大小
    let current_size_mb = partition.size_bytes / 1024 / 1024;
    let unallocated_after_mb =
        get_unallocated_space_after_partition_with_disk(disk, partition.partition_number);
    let max_size_mb = current_size_mb + unallocated_after_mb;

    // 如果没有可调整的空间
    if min_size_mb >= max_size_mb {
        return (
            false,
            tr!(
                "分区无法调整大小，已用空间 {} MB 接近分区大小 {} MB",
                used_mb,
                current_size_mb
            ),
            0,
            0,
        );
    }

    (
        true,
        tr!(
            "可调整范围: {} MB - {} MB (已用: {} MB)",
            min_size_mb,
            max_size_mb,
            used_mb
        ),
        min_size_mb,
        max_size_mb,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an explicit Windows integration test because it reads host drive-letter state"]
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

    #[test]
    fn adjacent_unallocated_bytes_are_not_floored_before_combining_with_partition_size() {
        const MIB: u64 = 1024 * 1024;
        let partition = DiskPartitionInfo {
            partition_number: 1,
            size_bytes: 100 * MIB + MIB / 2,
            offset_bytes: MIB,
            drive_letter: Some('D'),
            label: String::new(),
            file_system: "NTFS".into(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            partition_type: "basic".into(),
            used_bytes: 50 * MIB,
            free_bytes: 50 * MIB + MIB / 2,
            is_active: false,
        };
        let disk = PhysicalDisk {
            disk_number: 7,
            size_bytes: 102 * MIB,
            model: "test".into(),
            partition_style: PartitionStyle::GPT,
            is_initialized: true,
            partitions: vec![partition],
            unallocated_bytes: MIB / 2,
        };
        assert_eq!(
            get_unallocated_space_after_partition_bytes_with_disk(&disk, 1),
            MIB / 2
        );
        assert_eq!(
            (disk.partitions[0].size_bytes
                + get_unallocated_space_after_partition_bytes_with_disk(&disk, 1))
                / MIB,
            101
        );
        assert_eq!(get_unallocated_space_after_partition_with_disk(&disk, 1), 0);
    }
}
