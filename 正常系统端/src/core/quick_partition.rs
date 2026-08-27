//! 一键分区核心模块
//!
//! 提供磁盘分区的底层操作功能，所有查询和写入均使用文档化 Windows API。

use anyhow::{anyhow, Result};
use lr_core::data_staging::StorageAttachment;
use std::path::Path;

#[cfg(windows)]
use windows::{
    core::{HRESULT, PCWSTR},
    Win32::Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, GENERIC_READ, HANDLE,
    },
    Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
        IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_QUERY_PROPERTY,
        PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, PARTITION_STYLE_RAW, STORAGE_DESCRIPTOR_HEADER,
        STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
    },
    Win32::System::IO::DeviceIoControl,
};

/// IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS 常量
/// CTL_CODE(IOCTL_VOLUME_BASE, 0, METHOD_BUFFERED, FILE_ANY_ACCESS)
/// IOCTL_VOLUME_BASE = 0x56 ('V'), 所以值为 (0x56 << 16) | (0 << 14) | (0 << 2) | 0 = 0x00560000
#[cfg(windows)]
use crate::tr;

use super::disk::PartitionStyle;

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

/// One successful, read-only snapshot of a currently present SetupAPI disk interface.
///
/// Every field below is captured through one `GENERIC_READ` handle opened from the opaque
/// `SetupDiGetDeviceInterfaceDetailW` path. `disk_number` remains a current-session locator only.
#[derive(Debug, Clone)]
pub struct PresentPhysicalDisk {
    pub disk: PhysicalDisk,
    pub attachment: StorageAttachment,
    pub bus_type: u32,
    pub removable_media: bool,
    pub serial_number: String,
    pub firmware_revision: String,
    pub partition_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapacitySource {
    LengthInfo,
    GeometryEx,
    Vds,
}

fn select_capacity_source(
    length_info: Option<u64>,
    geometry_ex: Option<u64>,
    vds: Option<u64>,
) -> Option<(u64, CapacitySource)> {
    length_info
        .filter(|value| *value > 0)
        .map(|value| (value, CapacitySource::LengthInfo))
        .or_else(|| {
            geometry_ex
                .filter(|value| *value > 0)
                .map(|value| (value, CapacitySource::GeometryEx))
        })
        .or_else(|| {
            vds.filter(|value| *value > 0)
                .map(|value| (value, CapacitySource::Vds))
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DiskDeviceDescriptor {
    model: String,
    serial_number: String,
    firmware_revision: String,
    bus_type: u32,
    removable_media: bool,
}

impl PhysicalDisk {
    /// 获取磁盘大小（GB，保留1位小数）
    pub fn size_gb(&self) -> f64 {
        (self.size_bytes as f64 / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0
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

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct DiskLengthInfo {
    length: i64,
}

fn checked_disk_length(length: i64, bytes_returned: u32, required_size: usize) -> Option<u64> {
    (length > 0 && bytes_returned as usize >= required_size).then_some(length as u64)
}

/// DRIVE_LAYOUT_INFORMATION_EX 结构头部
#[cfg(windows)]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DriveLayoutInfoExHeader {
    partition_style: u32,
    partition_count: u32,
}

#[cfg(windows)]
fn is_variable_buffer_error(error: &windows::core::Error) -> bool {
    error.code() == HRESULT::from_win32(ERROR_MORE_DATA.0)
        || error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
}

#[cfg(windows)]
unsafe fn query_drive_layout(handle: HANDLE) -> Option<(Vec<u64>, u32)> {
    let mut capacity = 4096usize;
    loop {
        if capacity > 4 * 1024 * 1024 {
            return None;
        }
        let mut buffer = vec![0u64; capacity.div_ceil(std::mem::size_of::<u64>())];
        let mut returned = 0u32;
        match DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
            None,
            0,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * std::mem::size_of::<u64>()) as u32,
            Some(&mut returned),
            None,
        ) {
            Ok(()) => return Some((buffer, returned)),
            Err(error) if is_variable_buffer_error(&error) => capacity = capacity.checked_mul(2)?,
            Err(_) => return None,
        }
    }
}

fn storage_attachment(bus_type: u32, removable_media: bool) -> StorageAttachment {
    // STORAGE_BUS_TYPE values from the Windows SDK. Removable media and buses which normally
    // represent detachable devices must never be promoted to an internal full-disk target.
    const BUS_TYPE_IEEE1394: u32 = 4;
    const BUS_TYPE_USB: u32 = 7;
    const BUS_TYPE_SD: u32 = 12;
    const BUS_TYPE_MMC: u32 = 13;
    const BUS_TYPE_VIRTUAL: u32 = 14;
    const BUS_TYPE_FILE_BACKED_VIRTUAL: u32 = 15;

    if removable_media
        || matches!(
            bus_type,
            BUS_TYPE_IEEE1394 | BUS_TYPE_USB | BUS_TYPE_SD | BUS_TYPE_MMC
        )
    {
        StorageAttachment::External
    } else if matches!(bus_type, BUS_TYPE_VIRTUAL | BUS_TYPE_FILE_BACKED_VIRTUAL) {
        StorageAttachment::Unknown
    } else {
        StorageAttachment::Internal
    }
}

fn descriptor_text(buffer: &[u8], offset: u32, upper_bound: usize) -> String {
    let Ok(offset) = usize::try_from(offset) else {
        return String::new();
    };
    if offset == 0 || offset >= upper_bound || upper_bound > buffer.len() {
        return String::new();
    }
    let Some(end) = buffer[offset..upper_bound]
        .iter()
        .position(|byte| *byte == 0)
    else {
        return String::new();
    };
    String::from_utf8_lossy(&buffer[offset..offset + end])
        .trim()
        .to_string()
}

#[cfg(windows)]
unsafe fn query_disk_device_descriptor(handle: HANDLE) -> anyhow::Result<DiskDeviceDescriptor> {
    const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut header = STORAGE_DESCRIPTOR_HEADER::default();
    let mut returned = 0_u32;
    DeviceIoControl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
        std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        Some((&mut header as *mut STORAGE_DESCRIPTOR_HEADER).cast()),
        std::mem::size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32,
        Some(&mut returned),
        None,
    )
    .map_err(|error| anyhow!("读取存储设备描述符大小失败: {error}"))?;
    if returned < std::mem::size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32 {
        return Err(anyhow!("存储设备描述符大小响应不完整"));
    }
    let descriptor_size =
        usize::try_from(header.Size).map_err(|_| anyhow!("存储设备描述符大小超出支持范围"))?;
    if descriptor_size < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>()
        || descriptor_size > MAX_DESCRIPTOR_BYTES
    {
        return Err(anyhow!("存储设备描述符大小无效: {descriptor_size}"));
    }

    // u64 storage preserves the native alignment required by STORAGE_DEVICE_DESCRIPTOR.
    let mut storage = vec![0_u64; descriptor_size.div_ceil(std::mem::size_of::<u64>())];
    returned = 0;
    DeviceIoControl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
        std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        Some(storage.as_mut_ptr().cast()),
        (storage.len() * std::mem::size_of::<u64>()) as u32,
        Some(&mut returned),
        None,
    )
    .map_err(|error| anyhow!("读取存储设备描述符失败: {error}"))?;
    let returned = usize::try_from(returned).unwrap_or(usize::MAX);
    if returned < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>()
        || returned > storage.len() * std::mem::size_of::<u64>()
    {
        return Err(anyhow!("存储设备描述符响应长度无效: {returned}"));
    }
    let bytes = std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), returned);
    let descriptor = std::ptr::read_unaligned(bytes.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>());
    let declared = usize::try_from(descriptor.Size)
        .unwrap_or(usize::MAX)
        .min(returned);
    if declared < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return Err(anyhow!("存储设备描述符声明长度无效"));
    }
    let vendor = descriptor_text(bytes, descriptor.VendorIdOffset, declared);
    let product = descriptor_text(bytes, descriptor.ProductIdOffset, declared);
    let model = match (vendor, product) {
        (vendor, product)
            if !vendor.is_empty() && !product.is_empty() && !product.starts_with(&vendor) =>
        {
            format!("{vendor} {product}")
        }
        (_, product) if !product.is_empty() => product,
        (vendor, _) => vendor,
    };
    Ok(DiskDeviceDescriptor {
        model,
        serial_number: descriptor_text(bytes, descriptor.SerialNumberOffset, declared),
        firmware_revision: descriptor_text(bytes, descriptor.ProductRevisionOffset, declared),
        bus_type: descriptor.BusType.0 as u32,
        removable_media: descriptor.RemovableMedia.0 != 0,
    })
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
    match get_present_physical_disks() {
        Ok(disks) => disks,
        Err(error) => {
            log::error!("SetupAPI 物理磁盘枚举失败: {error}");
            Vec::new()
        }
    }
}

/// Enumerate present disk interfaces through SetupAPI, then query only those returned disk
/// numbers. This deliberately never probes a guessed `PhysicalDrive0..31` range.
#[cfg(windows)]
pub fn get_present_physical_disks() -> anyhow::Result<Vec<PhysicalDisk>> {
    Ok(get_present_physical_disk_inventory()?
        .into_iter()
        .map(|snapshot| snapshot.disk)
        .collect())
}

/// Capture current disk inventory from exact SetupAPI paths. A broken interface is isolated. The
/// list is collapsed by current-session disk number only after one exact-path handle produced the
/// capacity, dynamic layout and device descriptor needed by every inventory consumer.
#[cfg(windows)]
pub fn get_present_physical_disk_inventory() -> anyhow::Result<Vec<PresentPhysicalDisk>> {
    use std::collections::BTreeMap;

    let interfaces = lr_core::windows_storage::present_physical_disk_interfaces()
        .map_err(|error| anyhow::anyhow!("枚举当前物理磁盘接口失败: {error}"))?;
    let mut snapshots = BTreeMap::<u32, PresentPhysicalDisk>::new();
    for interface in interfaces {
        let candidate = match get_disk_info_from_path(interface.disk_number, &interface.device_path)
        {
            Ok(candidate) => candidate,
            Err(error) => {
                log::warn!(
                    "SetupAPI 磁盘接口 {} 的只读库存快照失败，已隔离该接口: {}",
                    interface.disk_number,
                    error
                );
                continue;
            }
        };
        snapshots.entry(interface.disk_number).or_insert(candidate);
    }
    if snapshots.is_empty() {
        return Err(anyhow!(
            "SetupAPI 返回了磁盘接口，但没有任何接口能提供完整的容量和分区布局快照"
        ));
    }
    Ok(snapshots.into_values().collect())
}

/// Read one physical disk through the same IOCTL-backed inventory used by the
/// quick-partition UI. Callers that already know the disk number should use
/// this instead of enumerating every possible disk.
#[cfg(windows)]
pub fn get_physical_disk(disk_number: u32) -> Option<PhysicalDisk> {
    get_disk_info(disk_number)
}

#[cfg(not(windows))]
pub fn get_physical_disk(_disk_number: u32) -> Option<PhysicalDisk> {
    None
}

#[cfg(not(windows))]
pub fn get_physical_disks() -> Vec<PhysicalDisk> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn get_present_physical_disks() -> anyhow::Result<Vec<PhysicalDisk>> {
    Ok(Vec::new())
}

#[cfg(not(windows))]
pub fn get_present_physical_disk_inventory() -> anyhow::Result<Vec<PresentPhysicalDisk>> {
    Ok(Vec::new())
}

/// 获取单个磁盘的详细信息
#[cfg(windows)]
fn get_disk_info(disk_number: u32) -> Option<PhysicalDisk> {
    get_present_physical_disk_inventory()
        .ok()?
        .into_iter()
        .find(|snapshot| snapshot.disk.disk_number == disk_number)
        .map(|snapshot| snapshot.disk)
}

#[cfg(windows)]
fn get_disk_info_from_path(
    disk_number: u32,
    disk_path: &str,
) -> anyhow::Result<PresentPhysicalDisk> {
    unsafe {
        let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();

        let handle = CreateFileW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map_err(|error| anyhow!("以只读方式打开当前 SetupAPI 磁盘接口失败: {error}"))?;

        let current_disk_number = lr_core::windows_storage::present_disk_number_from_handle(handle)
            .map_err(|error| {
                let _ = CloseHandle(handle);
                anyhow!("same-handle current disk-number query failed: {error}")
            })?;
        if current_disk_number != Some(disk_number) {
            let _ = CloseHandle(handle);
            return Err(anyhow!(
                "SetupAPI interface changed before snapshot: enumerated disk {disk_number}, same-handle current disk {current_disk_number:?}"
            ));
        }

        let mut length = DiskLengthInfo::default();
        let mut bytes_returned: u32 = 0;
        let length_result = DeviceIoControl(
            handle,
            IOCTL_DISK_GET_LENGTH_INFO,
            None,
            0,
            Some(&mut length as *mut _ as *mut _),
            std::mem::size_of::<DiskLengthInfo>() as u32,
            Some(&mut bytes_returned),
            None,
        );
        let length_bytes = length_result.as_ref().ok().and_then(|_| {
            checked_disk_length(
                length.length,
                bytes_returned,
                std::mem::size_of::<DiskLengthInfo>(),
            )
        });
        let length_context = match &length_result {
            Ok(()) => format!(
                "returned length {} in {} bytes",
                length.length, bytes_returned
            ),
            Err(error) => error.to_string(),
        };
        let mut geometry_bytes = None;
        let mut geometry_context = String::from("not attempted");
        if length_bytes.is_none() {
            let mut geometry = DiskGeometryEx::default();
            bytes_returned = 0;
            let geometry_result = DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                None,
                0,
                Some(&mut geometry as *mut _ as *mut _),
                std::mem::size_of::<DiskGeometryEx>() as u32,
                Some(&mut bytes_returned),
                None,
            );
            geometry_bytes = geometry_result.as_ref().ok().and_then(|_| {
                checked_disk_length(
                    geometry.disk_size,
                    bytes_returned,
                    std::mem::size_of::<DiskGeometryEx>(),
                )
            });
            geometry_context = match &geometry_result {
                Ok(()) => format!(
                    "returned length {} in {} bytes",
                    geometry.disk_size, bytes_returned
                ),
                Err(error) => error.to_string(),
            };
        }
        let (size_bytes, capacity_source) = match select_capacity_source(
            length_bytes,
            geometry_bytes,
            None,
        ) {
            Some(selected) => selected,
            None => match lr_core::windows_storage::vds_disk_size(disk_number) {
                Ok(size_bytes) => {
                    log::warn!(
                        "SetupAPI 磁盘接口的两个只读容量 IOCTL 均被驱动拒绝；仅容量字段回退到当前磁盘号 {disk_number} 的 VDS 值 {size_bytes} bytes。LENGTH_INFO=({length_context}); GEOMETRY_EX=({geometry_context})"
                    );
                    select_capacity_source(None, None, Some(size_bytes)).ok_or_else(|| {
                        let _ = CloseHandle(handle);
                        anyhow!("VDS returned a zero disk capacity")
                    })?
                }
                Err(vds_error) => {
                    let _ = CloseHandle(handle);
                    return Err(anyhow!(
                        "exact-path capacity queries failed and the same current disk number had no VDS capacity fallback: LENGTH_INFO=({length_context}); GEOMETRY_EX=({geometry_context}); VDS=({vds_error})"
                    ));
                }
            },
        };
        log::debug!(
            "SetupAPI disk interface {} capacity={} bytes source={capacity_source:?}",
            disk_path,
            size_bytes
        );

        let descriptor = query_disk_device_descriptor(handle).map_err(|error| {
            let _ = CloseHandle(handle);
            anyhow!("exact-path device descriptor query failed: {error}")
        })?;

        // DRIVE_LAYOUT_INFORMATION_EX is variable length. Microsoft requires retrying with a
        // larger buffer when the storage stack reports that the supplied buffer was too small.
        let layout = query_drive_layout(handle);
        let _ = CloseHandle(handle);

        let (buffer, returned) = layout.ok_or_else(|| {
            anyhow!("IOCTL_DISK_GET_DRIVE_LAYOUT_EX failed on the exact SetupAPI interface")
        })?;
        let (partition_style, is_initialized, partitions, partition_count) = {
            if returned < std::mem::size_of::<DriveLayoutInfoExHeader>() as u32 {
                return Err(anyhow!(
                    "IOCTL_DISK_GET_DRIVE_LAYOUT_EX response is truncated"
                ));
            }
            let bytes = std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), returned as usize);
            let header = std::ptr::read_unaligned(bytes.as_ptr().cast::<DriveLayoutInfoExHeader>());

            let style = match header.partition_style {
                x if x == PARTITION_STYLE_MBR.0 as u32 => PartitionStyle::MBR,
                x if x == PARTITION_STYLE_GPT.0 as u32 => PartitionStyle::GPT,
                x if x == PARTITION_STYLE_RAW.0 as u32 => PartitionStyle::Unknown,
                _ => PartitionStyle::Unknown,
            };

            let is_init = style != PartitionStyle::Unknown;

            // 解析分区信息
            let partitions = parse_partition_layout(bytes, &header, style, disk_number);

            (style, is_init, partitions, header.partition_count)
        };

        // 计算未分配空间
        let allocated: u64 = partitions.iter().map(|p| p.size_bytes).sum();
        let unallocated = size_bytes.saturating_sub(allocated);

        let attachment = storage_attachment(descriptor.bus_type, descriptor.removable_media);
        Ok(PresentPhysicalDisk {
            disk: PhysicalDisk {
                disk_number,
                size_bytes,
                model: descriptor.model,
                partition_style,
                is_initialized,
                partitions,
                unallocated_bytes: unallocated,
            },
            attachment,
            bus_type: descriptor.bus_type,
            removable_media: descriptor.removable_media,
            serial_number: descriptor.serial_number,
            firmware_revision: descriptor.firmware_revision,
            partition_count,
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
            if vol_disk == disk_number && vol_offset == offset {
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
    let identity = lr_core::windows_storage::volume_identity(letter).ok()?;
    Some((identity.disk_number, identity.offset_bytes))
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

/// Executes a plan whose physical-disk identity and current safety state were
/// already revalidated by the caller immediately before this boundary.
pub(crate) fn execute_quick_partition_validated(
    disk_number: u32,
    partition_style: PartitionStyle,
    layouts: &[PartitionLayout],
    expected_layout: &lr_core::windows_storage::DiskLayoutSnapshot,
) -> QuickPartitionResult {
    let style = match storage_style(partition_style) {
        Ok(style) => style,
        Err(error) => return quick_partition_failure(error),
    };
    if layouts.is_empty() {
        return quick_partition_failure(anyhow!("分区方案不能为空"));
    }
    if let Err(error) =
        lr_core::windows_storage::clean_and_initialize_checked(disk_number, expected_layout, style)
    {
        return quick_partition_failure(anyhow!("清除并初始化磁盘失败: {error}"));
    }
    let mut current_layout = match lr_core::windows_storage::disk_layout_snapshot(disk_number) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return quick_partition_failure(anyhow!(
                "初始化后无法重新确认目标磁盘稳定身份: {error}"
            ));
        }
    };

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
        match lr_core::windows_storage::create_partition_checked(&request, &current_layout) {
            Ok(_created) => {
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
                // The checked create boundary already performs its canonical postcondition
                // readback. Refresh only when another write still needs a fresh authorization
                // snapshot; a redundant whole-disk query after the final successful write can
                // only turn a completed operation into a false failure.
                if !is_last {
                    current_layout =
                        match lr_core::windows_storage::disk_layout_snapshot(disk_number) {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                return quick_partition_failure(anyhow!(
                                    "创建第 {} 个分区后无法重新确认目标磁盘稳定身份: {}",
                                    i + 1,
                                    error
                                ));
                            }
                        };
                }
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
    Ok(bytes.round() as u64)
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
    let Ok(mask) = lr_core::windows_storage::assigned_drive_letter_mask() else {
        return ('A'..='Z').collect();
    };
    (0u8..=25)
        .filter(|index| mask & (1u32 << index) != 0)
        .map(|index| char::from(b'A' + index))
        .collect()
}

/// 删除指定分区
pub fn delete_partition(disk_number: u32, partition_number: u32) -> Result<String> {
    let partition = current_partition(disk_number, partition_number)?;
    reject_running_system_partition(disk_number, &partition)?;
    let expected_layout = lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
    lr_core::windows_storage::delete_partition_checked(
        disk_number,
        partition.offset_bytes,
        true,
        &expected_layout,
    )?;
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
    let expected_extent = lr_core::windows_storage::VolumeIdentity {
        disk_number,
        offset_bytes: partition.offset_bytes,
        extent_length_bytes: partition.size_bytes,
    };
    let expected = lr_core::windows_storage::stable_volume_identity(letter)?;
    if !lr_core::windows_storage::same_volume_identity(expected.extent, expected_extent) {
        anyhow::bail!("分区稳定身份与当前库存不一致，已停止缩小");
    }
    let reclaimed = match lr_core::windows_storage::shrink_volume_stable_checked(
        letter, expected, requested, requested,
    ) {
        Ok(reclaimed) => reclaimed,
        Err(error) => {
            let recovery = recover_observed_quick_partition_shrink(letter, expected);
            anyhow::bail!("缩小分区失败: {error}; 恢复检查: {recovery}");
        }
    };
    Ok(tr!("分区已成功缩小 {} MB", reclaimed / MIB))
}

fn observed_stable_shrink_bytes(
    before: lr_core::windows_storage::StableVolumeIdentity,
    current: lr_core::windows_storage::StableVolumeIdentity,
) -> Result<Option<u64>> {
    if lr_core::windows_storage::same_stable_volume_identity(before, current) {
        return Ok(None);
    }
    if !lr_core::windows_storage::same_stable_partition_identity(before, current) {
        anyhow::bail!("当前盘符不再指向缩卷前认证的同一物理分区");
    }
    if current.extent.extent_length_bytes >= before.extent.extent_length_bytes {
        anyhow::bail!("当前分区范围没有形成可安全恢复的尾部缩小");
    }
    Ok(Some(
        before.extent.extent_length_bytes - current.extent.extent_length_bytes,
    ))
}

fn recover_observed_quick_partition_shrink(
    letter: char,
    before: lr_core::windows_storage::StableVolumeIdentity,
) -> String {
    let current = match lr_core::windows_storage::stable_volume_identity(letter) {
        Ok(current) => current,
        Err(error) => return format!("无法重新读取同一分区，未盲目扩容: {error}"),
    };
    let reclaimed = match observed_stable_shrink_bytes(before, current) {
        Ok(None) => return "权威回读显示分区范围未变化，无需恢复".to_owned(),
        Ok(Some(reclaimed)) => reclaimed,
        Err(error) => return format!("当前对象不是可证明的原分区尾部缩小，未盲目扩容: {error:#}"),
    };
    match lr_core::windows_storage::extend_volume_stable_checked(letter, current, reclaimed) {
        Ok(()) => match lr_core::windows_storage::stable_volume_identity(letter) {
            Ok(restored)
                if lr_core::windows_storage::same_stable_volume_identity(restored, before) =>
            {
                format!("已按权威回读恢复实际缩小的 {reclaimed} 字节")
            }
            Ok(restored) => format!(
                "已尝试恢复实际缩小范围，但最终权威回读不等于原范围: {:?}",
                restored.extent
            ),
            Err(error) => format!("已尝试恢复实际缩小范围，但最终回读失败: {error}"),
        },
        Err(error) => format!("观察到实际缩小 {reclaimed} 字节，但安全扩回失败: {error}"),
    }
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
    let expected_extent = lr_core::windows_storage::VolumeIdentity {
        disk_number,
        offset_bytes: partition.offset_bytes,
        extent_length_bytes: partition.size_bytes,
    };
    let expected = lr_core::windows_storage::stable_volume_identity(letter)?;
    if !lr_core::windows_storage::same_volume_identity(expected.extent, expected_extent) {
        anyhow::bail!("分区稳定身份与当前库存不一致，已停止扩展");
    }
    lr_core::windows_storage::extend_volume_stable_checked(letter, expected, requested)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_attachment_uses_descriptor_facts_without_guessing_virtual_disks() {
        assert_eq!(storage_attachment(7, false), StorageAttachment::External);
        assert_eq!(storage_attachment(11, true), StorageAttachment::External);
        assert_eq!(storage_attachment(14, false), StorageAttachment::Unknown);
        assert_eq!(storage_attachment(17, false), StorageAttachment::Internal);
    }

    #[test]
    fn capacity_fallback_order_never_replaces_a_successful_exact_path_result() {
        assert_eq!(
            select_capacity_source(Some(100), Some(200), Some(300)),
            Some((100, CapacitySource::LengthInfo))
        );
        assert_eq!(
            select_capacity_source(None, Some(200), Some(300)),
            Some((200, CapacitySource::GeometryEx))
        );
        assert_eq!(
            select_capacity_source(None, None, Some(300)),
            Some((300, CapacitySource::Vds))
        );
        assert_eq!(select_capacity_source(None, None, Some(0)), None);
    }

    #[test]
    fn descriptor_text_never_reads_past_the_returned_descriptor() {
        let bytes = b"\0\0\0\0MODEL\0TRAILING";
        assert_eq!(descriptor_text(bytes, 4, 10), "MODEL");
        assert!(descriptor_text(bytes, 4, 8).is_empty());
        assert!(descriptor_text(bytes, 99, bytes.len()).is_empty());
    }

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

    #[test]
    fn shrink_recovery_accepts_only_a_fresh_same_partition_tail_reduction() {
        let before = lr_core::windows_storage::StableVolumeIdentity {
            extent: lr_core::windows_storage::VolumeIdentity {
                disk_number: 7,
                offset_bytes: 1_048_576 + 512,
                extent_length_bytes: 90_000_000_321,
            },
            disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [7; 16] },
            partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                partition_id: [9; 16],
            },
            device_id_hash: Some([3; 32]),
        };
        let shrunk = lr_core::windows_storage::StableVolumeIdentity {
            extent: lr_core::windows_storage::VolumeIdentity {
                extent_length_bytes: before.extent.extent_length_bytes - 65_537,
                ..before.extent
            },
            ..before
        };
        assert_eq!(
            observed_stable_shrink_bytes(before, shrunk).unwrap(),
            Some(65_537)
        );
        assert_eq!(observed_stable_shrink_bytes(before, before).unwrap(), None);
        assert!(observed_stable_shrink_bytes(
            before,
            lr_core::windows_storage::StableVolumeIdentity {
                extent: lr_core::windows_storage::VolumeIdentity {
                    disk_number: 8,
                    ..shrunk.extent
                },
                ..shrunk
            }
        )
        .is_err());
        assert!(observed_stable_shrink_bytes(
            before,
            lr_core::windows_storage::StableVolumeIdentity {
                extent: lr_core::windows_storage::VolumeIdentity {
                    extent_length_bytes: before.extent.extent_length_bytes + 512,
                    ..before.extent
                },
                ..before
            }
        )
        .is_err());
    }
}
