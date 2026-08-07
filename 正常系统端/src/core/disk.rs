use crate::core::bitlocker::{BitLockerManager, VolumeStatus};
use crate::tr;
use anyhow::Result;
use lr_core::data_staging::{
    select_staging_plan, ShrinkCandidate, StagingCandidate, StagingPlan, StorageAttachment,
    StorageMedia,
};
use std::path::Path;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Win32::Storage::FileSystem::{
        CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
        IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_STYLE_GPT,
        PARTITION_STYLE_MBR, STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY,
    },
    Win32::System::IO::DeviceIoControl,
};

// 驱动器类型常量
#[allow(dead_code)]
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
#[allow(dead_code)]
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
#[allow(dead_code)]
const DRIVE_RAMDISK: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StagingDriveKind {
    Fixed,
    Removable,
}

fn classify_staging_drive_type(drive_type: u32) -> Option<StagingDriveKind> {
    match drive_type {
        DRIVE_FIXED => Some(StagingDriveKind::Fixed),
        DRIVE_REMOVABLE => Some(StagingDriveKind::Removable),
        _ => None,
    }
}

/// Windows supports shrinking an online NTFS BitLocker volume without decrypting it first.  Keep
/// that path limited to the current Windows volume while its conversion state is stable and the
/// volume is unlocked.  Locked, converting and unknown states remain fail-closed.
fn auto_shrink_target_is_safe(
    file_system: Option<&str>,
    bitlocker_status: VolumeStatus,
    attachment: StorageAttachment,
    is_current_system: bool,
) -> bool {
    let bitlocker_is_safe = bitlocker_status == VolumeStatus::NotEncrypted
        || (is_current_system && bitlocker_status == VolumeStatus::EncryptedUnlocked);
    bitlocker_is_safe
        && file_system.is_some_and(|name| name.eq_ignore_ascii_case("NTFS"))
        && attachment != StorageAttachment::External
}

/// 自动创建分区的标志文件名
pub const AUTO_CREATED_PARTITION_MARKER: &str = "LetRecovery_AutoCreated.marker";

/// 分区表类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// Keep the established names because they are shown verbatim throughout both endpoints.
#[allow(clippy::upper_case_acronyms)]
pub enum PartitionStyle {
    GPT,
    MBR,
    #[default]
    Unknown,
}

impl std::fmt::Display for PartitionStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionStyle::GPT => write!(f, "GPT"),
            PartitionStyle::MBR => write!(f, "MBR"),
            PartitionStyle::Unknown => write!(f, "{}", tr!("未知")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub letter: String,
    pub total_size_mb: u64,
    pub free_size_mb: u64,
    pub label: String,
    pub is_system_partition: bool,
    pub has_windows: bool,
    pub partition_style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
    /// Stable geometry captured from the physical-disk IOCTL inventory.
    pub disk_size_bytes: Option<u64>,
    pub partition_offset_bytes: Option<u64>,
    pub partition_size_bytes: Option<u64>,
    pub bitlocker_status: VolumeStatus,
}

/// 分区详细信息
#[derive(Debug, Clone)]
pub struct PartitionDetail {
    pub style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
}

/// STORAGE_DEVICE_NUMBER 结构
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

/// DRIVE_LAYOUT_INFORMATION_EX 结构（简化版，只需要头部信息）
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct DriveLayoutInformationEx {
    partition_style: u32,
    partition_count: u32,
    // union 部分我们不需要完整读取
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
#[allow(non_snake_case)]
struct DeviceSeekPenaltyDescriptor {
    Version: u32,
    Size: u32,
    IncursSeekPenalty: u8,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
#[allow(non_snake_case)]
struct StorageDeviceDescriptor {
    Version: u32,
    Size: u32,
    DeviceType: u8,
    DeviceTypeModifier: u8,
    RemovableMedia: u8,
    CommandQueueing: u8,
    VendorIdOffset: u32,
    ProductIdOffset: u32,
    ProductRevisionOffset: u32,
    SerialNumberOffset: u32,
    BusType: u32,
    RawPropertiesLength: u32,
}

pub struct DiskManager;

impl DiskManager {
    /// 获取所有固定磁盘分区列表
    pub fn get_partitions() -> Result<Vec<Partition>> {
        let mut partitions = Vec::new();
        let is_pe = Self::is_pe_environment();
        let running_windows_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;

        // 预先创建 BitLockerManager 实例，避免重复创建
        let bitlocker_manager = BitLockerManager::new();

        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            if let Ok(info) =
                Self::get_partition_info(&drive, is_pe, running_windows_drive, &bitlocker_manager)
            {
                partitions.push(info);
            }
        }

        // Disk and partition numbers can be reused after a hot-plug or a
        // delete/recreate cycle. Enrich every visible volume with immutable
        // geometry from the physical disk before any install intent is built.
        let mut disk_geometry = std::collections::BTreeMap::new();
        for partition in &partitions {
            if let Some(disk_number) = partition.disk_number {
                disk_geometry.entry(disk_number).or_insert_with(|| {
                    crate::core::quick_partition::get_physical_disk(disk_number)
                });
            }
        }
        for partition in &mut partitions {
            let (Some(disk_number), Some(partition_number)) =
                (partition.disk_number, partition.partition_number)
            else {
                continue;
            };
            let Some(Some(disk)) = disk_geometry.get(&disk_number) else {
                continue;
            };
            let Some(physical_partition) = disk
                .partitions
                .iter()
                .find(|candidate| candidate.partition_number == partition_number)
            else {
                continue;
            };
            partition.disk_size_bytes = Some(disk.size_bytes);
            partition.partition_offset_bytes = Some(physical_partition.offset_bytes);
            partition.partition_size_bytes = Some(physical_partition.size_bytes);
        }

        Ok(partitions)
    }

    fn get_partition_info(
        drive: &str,
        is_pe: bool,
        running_windows_drive: char,
        bitlocker_manager: &BitLockerManager,
    ) -> Result<Partition> {
        Self::get_partition_info_for_staging(
            drive,
            is_pe,
            running_windows_drive,
            bitlocker_manager,
            false,
        )
        .map(|(partition, _)| partition)
    }

    fn get_partition_info_for_staging(
        drive: &str,
        is_pe: bool,
        running_windows_drive: char,
        bitlocker_manager: &BitLockerManager,
        include_removable: bool,
    ) -> Result<(Partition, StagingDriveKind)> {
        let path = format!("{}\\", drive);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let drive_kind = {
            #[cfg(windows)]
            {
                // 光驱、虚拟光驱、网络盘、RAM 盘和无法识别的卷不允许承载安装数据。
                // USB 移动 SSD 可能被 Windows 报为 DRIVE_FIXED，后续还会结合总线类型降级。
                let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };
                let kind = classify_staging_drive_type(drive_type)
                    .ok_or_else(|| anyhow::anyhow!("Unsupported staging drive type"))?;
                if kind == StagingDriveKind::Removable && !include_removable {
                    anyhow::bail!("Not a fixed drive");
                }
                kind
            }
            #[cfg(not(windows))]
            {
                StagingDriveKind::Fixed
            }
        };

        // 获取磁盘空间
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        #[cfg(windows)]
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available as *mut u64),
                Some(&mut total_bytes as *mut u64),
                Some(&mut total_free_bytes as *mut u64),
            )?;
        }

        // 获取卷标
        let mut volume_name = [0u16; 261];
        #[cfg(windows)]
        unsafe {
            let _ = GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut volume_name),
                None,
                None,
                None,
                None,
            );
        }
        let label = String::from_utf16_lossy(&volume_name)
            .trim_end_matches('\0')
            .to_string();

        // 检查是否为当前系统分区
        let is_current_system = drive
            .chars()
            .next()
            .is_some_and(|letter| letter.eq_ignore_ascii_case(&running_windows_drive));

        // 检查是否包含 Windows 系统
        let windows_path = format!("{}\\Windows\\System32", drive);
        let has_windows = Path::new(&windows_path).exists();

        // 在 PE 环境下，is_system_partition 表示是否包含 Windows
        // 在正常环境下，is_system_partition 表示是否是当前系统盘
        let is_system_partition = if is_pe {
            has_windows && !is_current_system // PE下排除 X: 盘
        } else {
            is_current_system
        };

        // 获取分区表类型、磁盘号和分区号
        let detail = Self::get_partition_style(drive);

        // 获取 BitLocker 状态
        let letter_char = drive.chars().next().unwrap_or('C');
        let bitlocker_status = bitlocker_manager.get_status(letter_char);

        Ok((
            Partition {
                letter: drive.to_string(),
                total_size_mb: total_bytes / 1024 / 1024,
                free_size_mb: free_bytes_available / 1024 / 1024,
                label,
                is_system_partition,
                has_windows,
                partition_style: detail.style,
                disk_number: detail.disk_number,
                partition_number: detail.partition_number,
                disk_size_bytes: None,
                partition_offset_bytes: None,
                partition_size_bytes: None,
                bitlocker_status,
            },
            drive_kind,
        ))
    }

    /// 使用 Windows API 获取分区表类型和分区号 (GPT/MBR)
    #[cfg(windows)]
    fn get_partition_style(drive: &str) -> PartitionDetail {
        let letter = drive.chars().next().unwrap_or('C');

        // 先获取磁盘号和分区号
        let (disk_number, partition_number) = Self::get_device_number(letter);

        // 再获取分区表类型
        let style = if let Some(disk_num) = disk_number {
            Self::get_disk_partition_style_api(disk_num)
        } else {
            PartitionStyle::Unknown
        };

        PartitionDetail {
            style,
            disk_number,
            partition_number,
        }
    }

    #[cfg(not(windows))]
    fn get_partition_style(_drive: &str) -> PartitionDetail {
        PartitionDetail {
            style: PartitionStyle::Unknown,
            disk_number: None,
            partition_number: None,
        }
    }

    /// 使用 IOCTL_STORAGE_GET_DEVICE_NUMBER 获取磁盘号和分区号
    #[cfg(windows)]
    fn get_device_number(letter: char) -> (Option<u32>, Option<u32>) {
        unsafe {
            // 打开卷设备
            let volume_path = format!("\\\\.\\{}:", letter);
            let wide_path: Vec<u16> = volume_path
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0, // 不需要读写权限，只需要查询
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return (None, None),
            };

            if handle == INVALID_HANDLE_VALUE {
                return (None, None);
            }

            let mut device_number = StorageDeviceNumber::default();
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                None,
                0,
                Some(&mut device_number as *mut _ as *mut _),
                std::mem::size_of::<StorageDeviceNumber>() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_ok() {
                (
                    Some(device_number.device_number),
                    Some(device_number.partition_number),
                )
            } else {
                (None, None)
            }
        }
    }

    #[cfg(windows)]
    fn get_storage_profile(disk_number: u32) -> (StorageMedia, StorageAttachment) {
        const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: i32 = 7;
        const BUS_TYPE_IEEE1394: u32 = 4;
        const BUS_TYPE_USB: u32 = 7;
        const BUS_TYPE_SD: u32 = 12;
        const BUS_TYPE_MMC: u32 = 13;
        const BUS_TYPE_VIRTUAL: u32 = 14;
        const BUS_TYPE_FILE_BACKED_VIRTUAL: u32 = 15;
        const BUS_TYPE_NVME: u32 = 17;
        const BUS_TYPE_SCM: u32 = 18;

        unsafe {
            let disk_path = format!("\\\\.\\PhysicalDrive{disk_number}");
            let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();
            let handle = match CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            ) {
                Ok(handle) if handle != INVALID_HANDLE_VALUE => handle,
                _ => return (StorageMedia::Unknown, StorageAttachment::Unknown),
            };

            let mut query = STORAGE_PROPERTY_QUERY {
                PropertyId: STORAGE_PROPERTY_ID(STORAGE_DEVICE_SEEK_PENALTY_PROPERTY),
                QueryType: PropertyStandardQuery,
                AdditionalParameters: [0],
            };
            let mut seek = DeviceSeekPenaltyDescriptor::default();
            let mut bytes_returned = 0;
            let seek_result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const std::ffi::c_void),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(&mut seek as *mut _ as *mut std::ffi::c_void),
                std::mem::size_of::<DeviceSeekPenaltyDescriptor>() as u32,
                Some(&mut bytes_returned),
                None,
            );
            let seek_penalty = (seek_result.is_ok()
                && bytes_returned >= std::mem::size_of::<DeviceSeekPenaltyDescriptor>() as u32)
                .then_some(seek.IncursSeekPenalty != 0);

            query.PropertyId = StorageDeviceProperty;
            let mut descriptor = StorageDeviceDescriptor::default();
            bytes_returned = 0;
            let descriptor_result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const std::ffi::c_void),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(&mut descriptor as *mut _ as *mut std::ffi::c_void),
                std::mem::size_of::<StorageDeviceDescriptor>() as u32,
                Some(&mut bytes_returned),
                None,
            );
            let _ = CloseHandle(handle);

            let bus_type = (descriptor_result.is_ok()
                && bytes_returned >= std::mem::size_of::<StorageDeviceDescriptor>() as u32)
                .then_some(descriptor.BusType);
            let attachment = match bus_type {
                Some(BUS_TYPE_IEEE1394 | BUS_TYPE_USB | BUS_TYPE_SD | BUS_TYPE_MMC) => {
                    StorageAttachment::External
                }
                Some(BUS_TYPE_VIRTUAL | BUS_TYPE_FILE_BACKED_VIRTUAL) | None => {
                    StorageAttachment::Unknown
                }
                Some(_) => StorageAttachment::Internal,
            };
            let media = match seek_penalty {
                Some(true) => StorageMedia::Rotational,
                Some(false) => StorageMedia::SolidState,
                None if matches!(bus_type, Some(BUS_TYPE_NVME | BUS_TYPE_SCM)) => {
                    StorageMedia::SolidState
                }
                None => StorageMedia::Unknown,
            };
            (media, attachment)
        }
    }

    #[cfg(not(windows))]
    fn get_storage_profile(_disk_number: u32) -> (StorageMedia, StorageAttachment) {
        (StorageMedia::Unknown, StorageAttachment::Unknown)
    }

    #[cfg(windows)]
    fn get_volume_space_bytes(letter: char) -> Option<(u64, u64)> {
        let path = format!("{}:\\", letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) } == DRIVE_CDROM {
            return None;
        }
        let mut free_bytes_available = 0;
        let mut total_bytes = 0;
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available),
                Some(&mut total_bytes),
                None,
            )
            .ok()
            .map(|_| (free_bytes_available, total_bytes))
        }
    }

    #[cfg(windows)]
    fn volume_is_writable(letter: char) -> bool {
        const FILE_READ_ONLY_VOLUME_FLAG: u32 = 0x0008_0000;

        let path = format!("{}:\\", letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut file_system_flags = 0u32;
        unsafe {
            GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                None,
                None,
                None,
                Some(&mut file_system_flags),
                None,
            )
            .is_ok()
                && file_system_flags & FILE_READ_ONLY_VOLUME_FLAG == 0
        }
    }

    #[cfg(not(windows))]
    fn volume_is_writable(_letter: char) -> bool {
        true
    }

    fn get_staging_partitions() -> Result<Vec<(Partition, StagingDriveKind)>> {
        let mut partitions = Vec::new();
        let is_pe = Self::is_pe_environment();
        let running_windows_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;
        let bitlocker_manager = BitLockerManager::new();

        for letter in b'A'..=b'Z' {
            let letter = letter as char;
            let drive = format!("{letter}:");
            let Ok((partition, drive_kind)) = Self::get_partition_info_for_staging(
                &drive,
                is_pe,
                running_windows_drive,
                &bitlocker_manager,
                true,
            ) else {
                continue;
            };
            if !Self::volume_is_writable(letter) {
                log::warn!(
                    "[DISK] 跳过 {}:：卷只读或无法确认可写，不能承载 ViaPE 安装数据",
                    letter
                );
                continue;
            }
            partitions.push((partition, drive_kind));
        }

        Ok(partitions)
    }

    #[cfg(not(windows))]
    fn get_volume_space_bytes(_letter: char) -> Option<(u64, u64)> {
        None
    }

    #[cfg(windows)]
    fn get_volume_file_system(letter: char) -> Option<String> {
        let path = format!("{}:\\", letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut file_system_name = [0u16; 64];
        unsafe {
            GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut file_system_name),
            )
            .ok()?;
        }
        Some(
            String::from_utf16_lossy(&file_system_name)
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    #[cfg(not(windows))]
    fn get_volume_file_system(_letter: char) -> Option<String> {
        None
    }

    /// 使用 IOCTL_DISK_GET_DRIVE_LAYOUT_EX 获取磁盘分区表类型
    #[cfg(windows)]
    fn get_disk_partition_style_api(disk_number: u32) -> PartitionStyle {
        unsafe {
            // 打开物理磁盘
            let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_number);
            let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0, // 不需要读写权限
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return PartitionStyle::Unknown,
            };

            if handle == INVALID_HANDLE_VALUE {
                return PartitionStyle::Unknown;
            }

            // 分配足够大的缓冲区来存储分区布局信息
            // DRIVE_LAYOUT_INFORMATION_EX 的大小取决于分区数量
            // 我们只需要头部的 partition_style 字段
            let mut buffer = vec![0u8; 4096];
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
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

            if result.is_ok() && bytes_returned >= 8 {
                // 读取头部的 partition_style 字段（前4字节）
                let layout = &*(buffer.as_ptr() as *const DriveLayoutInformationEx);

                match layout.partition_style {
                    x if x == PARTITION_STYLE_MBR.0 as u32 => PartitionStyle::MBR,
                    x if x == PARTITION_STYLE_GPT.0 as u32 => PartitionStyle::GPT,
                    _ => PartitionStyle::Unknown,
                }
            } else {
                PartitionStyle::Unknown
            }
        }
    }

    /// 格式化指定分区
    pub fn format_partition(partition: &str) -> Result<String> {
        lr_core::format_command::FormatCommandSpec::new(partition, "NTFS", None)
            .map_err(|error| anyhow::anyhow!("无效的格式化参数: {error}"))?;
        let drive_letter = partition
            .trim()
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("格式化目标缺少盘符"))?;
        lr_core::windows_storage::format_drive(
            drive_letter,
            lr_core::windows_storage::FileSystem::Ntfs,
            "",
        )
        .map_err(|error| anyhow::anyhow!("格式化分区失败: {error}"))?;
        Ok("format completed".to_owned())
    }

    /// 从指定分区缩小并创建新分区
    pub fn shrink_and_create_partition(
        source_partition: &str,
        new_letter: &str,
        size_mb: u64,
    ) -> Result<String> {
        let source = source_partition
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("源分区盘符为空"))?
            .to_ascii_uppercase();
        let target = new_letter
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("新分区盘符为空"))?
            .to_ascii_uppercase();
        let (disk_number, _) = Self::get_device_number(source);
        let disk_number =
            disk_number.ok_or_else(|| anyhow::anyhow!("无法确认源分区 {}: 所在磁盘", source))?;
        let bytes = size_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| anyhow::anyhow!("分区大小超出支持范围"))?;
        lr_core::windows_storage::shrink_volume(source, bytes, bytes)?;
        let created = lr_core::windows_storage::create_partition(
            &lr_core::windows_storage::CreatePartitionRequest {
                disk_number,
                offset_bytes: 0,
                size_bytes: bytes,
                kind: lr_core::windows_storage::PartitionKind::BasicData,
                file_system: Some(lr_core::windows_storage::FileSystem::Ntfs),
                label: String::new(),
                drive_letter: Some(target),
                active: false,
                preserve_gpt_metadata: None,
            },
        );
        match created {
            Ok(_) => Ok(format!("{target}:")),
            Err(error) => {
                let rollback = lr_core::windows_storage::extend_volume(source, disk_number, bytes);
                Err(anyhow::anyhow!(
                    "缩小源分区后创建新分区失败: {error}; 回滚扩容结果: {}",
                    rollback
                        .map(|_| "成功".to_string())
                        .unwrap_or_else(|rollback_error| format!("失败: {rollback_error}"))
                ))
            }
        }
    }

    /// 删除指定分区
    pub fn delete_partition(partition_letter: &str) -> Result<String> {
        let letter = partition_letter
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("分区盘符为空"))?
            .to_ascii_uppercase();
        let (disk_number, partition_number) = Self::get_device_number(letter);
        let disk_number =
            disk_number.ok_or_else(|| anyhow::anyhow!("无法确认分区 {}: 所在磁盘", letter))?;
        let partition_number =
            partition_number.ok_or_else(|| anyhow::anyhow!("无法确认分区 {}: 的分区号", letter))?;
        super::quick_partition::delete_partition(disk_number, partition_number)
    }

    /// 检查指定分区是否包含有效的 Windows 系统
    pub fn has_valid_windows(partition: &str) -> bool {
        let paths_to_check = [
            format!("{}\\Windows\\System32\\config\\SYSTEM", partition),
            format!("{}\\Windows\\System32\\config\\SOFTWARE", partition),
            format!("{}\\Windows\\explorer.exe", partition),
        ];

        paths_to_check.iter().all(|p| Path::new(p).exists())
    }

    /// 获取 Windows 版本信息（使用 Windows API）
    #[cfg(windows)]
    pub fn get_windows_version(partition: &str) -> Option<String> {
        use windows::Win32::Storage::FileSystem::GetFileVersionInfoSizeW;
        use windows::Win32::Storage::FileSystem::GetFileVersionInfoW;
        use windows::Win32::Storage::FileSystem::VerQueryValueW;

        let ntoskrnl = format!("{}\\Windows\\System32\\ntoskrnl.exe", partition);
        if !Path::new(&ntoskrnl).exists() {
            return None;
        }

        unsafe {
            let wide_path: Vec<u16> = ntoskrnl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut handle: u32 = 0;

            let size =
                GetFileVersionInfoSizeW(PCWSTR::from_raw(wide_path.as_ptr()), Some(&mut handle));
            if size == 0 {
                return None;
            }

            let mut buffer = vec![0u8; size as usize];
            let result = GetFileVersionInfoW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0,
                size,
                buffer.as_mut_ptr() as *mut _,
            );

            if result.is_err() {
                return None;
            }

            // 查询固定文件信息
            let sub_block: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
            let mut info_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut info_len: u32 = 0;

            let result = VerQueryValueW(
                buffer.as_ptr() as *const _,
                PCWSTR::from_raw(sub_block.as_ptr()),
                &mut info_ptr,
                &mut info_len,
            );

            if result.as_bool() && !info_ptr.is_null() {
                // VS_FIXEDFILEINFO 结构
                #[repr(C)]
                struct VsFixedFileInfo {
                    dw_signature: u32,
                    dw_struc_version: u32,
                    dw_file_version_ms: u32,
                    dw_file_version_ls: u32,
                    dw_product_version_ms: u32,
                    dw_product_version_ls: u32,
                    // ... 其他字段我们不需要
                }

                let info = &*(info_ptr as *const VsFixedFileInfo);
                let major = (info.dw_file_version_ms >> 16) & 0xFFFF;
                let minor = info.dw_file_version_ms & 0xFFFF;
                let build = (info.dw_file_version_ls >> 16) & 0xFFFF;
                let revision = info.dw_file_version_ls & 0xFFFF;

                return Some(format!("{}.{}.{}.{}", major, minor, build, revision));
            }

            None
        }
    }

    #[cfg(not(windows))]
    pub fn get_windows_version(_partition: &str) -> Option<String> {
        None
    }

    pub fn is_pe_environment() -> bool {
        crate::core::system_info::SystemInfo::check_pe_environment()
    }

    /// 检查指定盘符是否为光驱
    #[cfg(windows)]
    pub fn is_cdrom(letter: char) -> bool {
        let path = format!("{}:\\", letter);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let drive_type = GetDriveTypeW(PCWSTR(wide_path.as_ptr()));
            drive_type == DRIVE_CDROM
        }
    }

    #[cfg(not(windows))]
    pub fn is_cdrom(_letter: char) -> bool {
        false
    }

    /// 检查指定盘符是否为固定磁盘
    #[cfg(windows)]
    pub fn is_fixed_drive(letter: char) -> bool {
        let path = format!("{}:\\", letter);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let drive_type = GetDriveTypeW(PCWSTR(wide_path.as_ptr()));
            drive_type == DRIVE_FIXED
        }
    }

    #[cfg(not(windows))]
    pub fn is_fixed_drive(_letter: char) -> bool {
        false
    }

    /// 获取指定分区的剩余空间（字节）
    #[cfg(windows)]
    pub fn get_free_space_bytes(partition: &str) -> Option<u64> {
        let path = format!("{}\\", partition);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        if unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) } == DRIVE_CDROM {
            return None;
        }

        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        unsafe {
            let result = GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available as *mut u64),
                Some(&mut total_bytes as *mut u64),
                Some(&mut total_free_bytes as *mut u64),
            );

            if result.is_ok() {
                Some(free_bytes_available)
            } else {
                None
            }
        }
    }

    #[cfg(not(windows))]
    pub fn get_free_space_bytes(_partition: &str) -> Option<u64> {
        None
    }

    /// 获取所有已使用的盘符
    pub fn get_used_drive_letters() -> Vec<char> {
        let Ok(mask) = lr_core::windows_storage::assigned_drive_letter_mask() else {
            // A failed inventory must not cause an already assigned letter to
            // be reused by a destructive storage operation.
            return ('A'..='Z').collect();
        };
        (0u8..=25)
            .filter(|index| mask & (1u32 << index) != 0)
            .map(|index| char::from(b'A' + index))
            .collect()
    }

    /// 查找第一个可用的盘符（未被使用的）
    pub fn find_available_drive_letter() -> Option<char> {
        let used = Self::get_used_drive_letters();
        // 从后往前找，避开常用盘符
        for letter in ('E'..='Z').rev() {
            if !used.contains(&letter) {
                return Some(letter);
            }
        }
        // 如果都被占用，尝试 D
        if !used.contains(&'D') {
            return Some('D');
        }
        None
    }

    /// 查询指定分区可缩小的最大空间（MB）
    pub fn query_shrink_max(letter: char) -> Result<u64> {
        let max_mb = lr_core::windows_storage::query_max_reclaimable_bytes(letter)? / 1024 / 1024;
        log::info!("[DISK] 分区 {}: 可缩小的最大空间: {} MB", letter, max_mb);
        Ok(max_mb)
    }

    /// 解析 shrink querymax 输出（英文）
    fn parse_shrink_max_output(output: &str) -> Option<u64> {
        // 匹配 "XXX MB" 或 "XXX GB" 格式
        for line in output.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("reclaimable") || line_lower.contains("maximum") {
                // 提取数字
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if let Ok(num) = part.replace(",", "").parse::<u64>() {
                        // 检查单位
                        if i + 1 < parts.len() {
                            let unit = parts[i + 1].to_lowercase();
                            if unit.starts_with("gb") {
                                return Some(num * 1024);
                            } else if unit.starts_with("mb") {
                                return Some(num);
                            }
                        }
                        return Some(num); // 默认 MB
                    }
                }
            }
        }
        None
    }

    /// 解析 shrink querymax 输出（中文）
    fn parse_shrink_max_output_cn(output: &str) -> Option<u64> {
        for line in output.lines() {
            // 中文输出可能的格式：
            // "可回收的最大字节数为:  XXX MB"
            // "最多可从此卷收回 XXX MB"
            // "可以从该卷收回的最大空间是: XXX MB"
            // "该卷可以收回的最大空间为 XXX MB"
            if line.contains("回收")
                || line.contains("收回")
                || line.contains("可用")
                || line.contains("压缩")
                || line.contains("缩小")
                || line.contains("最大")
                || line.contains("空间")
                || line.contains("字节")
            {
                log::info!("[DISK] 尝试解析中文行: {}", line);
                if let Some(size) = Self::extract_size_from_line(line) {
                    log::info!("[DISK] 解析成功: {} MB", size);
                    return Some(size);
                }
            }
        }
        None
    }

    /// 通用解析：查找任何包含数字+MB/GB的行
    fn parse_shrink_max_generic(output: &str) -> Option<u64> {
        for line in output.lines() {
            // 跳过明显的非结果行
            let line_lower = line.to_lowercase();
            if line_lower.contains("diskpart")
                || line_lower.contains("microsoft")
                || line_lower.contains("version")
                || line_lower.contains("volume")
                || line_lower.contains("select")
                || line.trim().is_empty()
            {
                continue;
            }

            if let Some(size) = Self::extract_size_from_line(line) {
                return Some(size);
            }
        }
        None
    }

    /// 从一行文本中提取大小（MB）
    fn extract_size_from_line(line: &str) -> Option<u64> {
        let mut num_str = String::new();
        let mut found_num = false;
        let chars: Vec<char> = line.chars().collect();

        for (i, c) in chars.iter().enumerate() {
            if c.is_ascii_digit() {
                num_str.push(*c);
                found_num = true;
            } else if found_num && *c == ',' {
                // 跳过千位分隔符
                continue;
            } else if found_num && !c.is_ascii_digit() {
                // 数字结束，检查单位
                if let Ok(num) = num_str.replace(",", "").parse::<u64>() {
                    if num == 0 {
                        num_str.clear();
                        found_num = false;
                        continue;
                    }
                    // 查找后面的单位
                    let rest: String = chars[i..].iter().collect();
                    let rest_lower = rest.to_lowercase();
                    if rest_lower.starts_with(" gb") || rest_lower.starts_with("gb") {
                        return Some(num * 1024);
                    } else if rest_lower.starts_with(" mb") || rest_lower.starts_with("mb") {
                        return Some(num);
                    } else if rest_lower.starts_with(" kb") || rest_lower.starts_with("kb") {
                        return Some(num / 1024);
                    }
                    // 如果数字较大（>100），假设是 MB
                    if num > 100 {
                        return Some(num);
                    }
                }
                num_str.clear();
                found_num = false;
            }
        }

        // 如果循环结束还有数字
        if !num_str.is_empty() {
            if let Ok(num) = num_str.parse::<u64>() {
                if num > 100 {
                    return Some(num);
                }
            }
        }

        None
    }

    /// 从指定分区缩小并创建新分区（增强版，带标志文件）
    ///
    /// # Arguments
    /// * `source_letter` - 源分区盘符
    /// * `desired_size_mb` - 期望的新分区大小（MB）
    /// * `pre_queried_max_mb` - 预先查询的最大可缩小空间（MB），如果为 None 则内部查询
    ///
    /// # Returns
    /// * `Ok(char)` - 新分区的盘符
    /// * `Err` - 错误信息
    pub fn shrink_and_create_partition_with_marker(
        source_letter: char,
        desired_size_mb: u64,
        pre_queried_max_mb: Option<u64>,
        expected_disk_number: u32,
        expected_partition_number: u32,
        expected_bitlocker_status: VolumeStatus,
        is_current_system: bool,
    ) -> Result<char> {
        let source_letter = source_letter.to_ascii_uppercase();
        let current_identity = Self::get_device_number(source_letter);
        if current_identity != (Some(expected_disk_number), Some(expected_partition_number)) {
            anyhow::bail!(
                "分区身份已变化，拒绝缩小 {}:：预期磁盘 {} 分区 {}，当前为 {:?}",
                source_letter,
                expected_disk_number,
                expected_partition_number,
                current_identity
            );
        }

        let refreshed_bitlocker_status = BitLockerManager::new().get_status(source_letter);
        if refreshed_bitlocker_status != expected_bitlocker_status
            || !matches!(
                refreshed_bitlocker_status,
                VolumeStatus::NotEncrypted | VolumeStatus::EncryptedUnlocked
            )
            || (refreshed_bitlocker_status == VolumeStatus::EncryptedUnlocked && !is_current_system)
        {
            anyhow::bail!(
                "BitLocker 状态已变化或不允许自动缩卷，拒绝缩小 {}:：预期={}，当前={}，当前系统卷={}",
                source_letter,
                expected_bitlocker_status.as_str(),
                refreshed_bitlocker_status.as_str(),
                is_current_system
            );
        }
        if refreshed_bitlocker_status == VolumeStatus::EncryptedUnlocked {
            log::info!(
                "[DISK] {}: 为已解锁且转换状态稳定的 BitLocker 当前系统卷，使用 Windows 原生缩卷而不执行完整解密",
                source_letter
            );
        }

        // 使用预查询的值或者重新查询
        let max_shrink_mb = match pre_queried_max_mb {
            Some(mb) => mb,
            None => Self::query_shrink_max(source_letter)?,
        };

        if max_shrink_mb == 0 {
            anyhow::bail!(
                "{}",
                tr!(
                    "分区 {}: 无法缩小，可能需要先进行碎片整理。\n\
                建议：在 Windows 中运行磁盘碎片整理工具，或使用其他分区工具。",
                    source_letter
                )
            );
        }

        // 使用实际可缩小的空间
        let actual_size_mb = if desired_size_mb > max_shrink_mb {
            log::warn!(
                "[DISK] 警告: 期望缩小 {} MB，但最多只能缩小 {} MB，将使用最大可用值",
                desired_size_mb,
                max_shrink_mb
            );
            max_shrink_mb
        } else {
            desired_size_mb
        };

        // 确保至少有 1GB 可用
        if actual_size_mb < 1024 {
            anyhow::bail!(
                "{}",
                tr!(
                    "分区 {}: 可缩小空间太小（{} MB），需要至少 1024 MB (1 GB)。\n\
                建议：清理磁盘空间或进行碎片整理后重试。",
                    source_letter,
                    actual_size_mb
                )
            );
        }

        // 找一个可用的盘符
        let new_letter = Self::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有可用的盘符")))?;

        log::info!(
            "[DISK] 准备从 {}: 缩小 {} MB 并创建新分区 {}:",
            source_letter,
            actual_size_mb,
            new_letter
        );

        let result = Self::shrink_and_create_partition(
            &source_letter.to_string(),
            &new_letter.to_string(),
            actual_size_mb,
        )?;
        log::info!(
            "[DISK] WinAPI 已从磁盘 {} 分区 {} 创建暂存卷 {}",
            expected_disk_number,
            expected_partition_number,
            result
        );

        // 等待系统识别新分区
        std::thread::sleep(std::time::Duration::from_secs(2));

        // 验证新分区是否创建成功
        let new_partition_path = format!("{}:\\", new_letter);
        for retry in 0..5 {
            if Path::new(&new_partition_path).exists() {
                break;
            }
            if retry == 4 {
                anyhow::bail!("{}", tr!("分区创建失败：新分区 {}: 不可访问。", new_letter));
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        let new_identity = Self::get_device_number(new_letter);
        if new_identity.0 != Some(expected_disk_number) {
            anyhow::bail!(
                "新分区 {}: 出现在错误的物理磁盘上：预期磁盘 {}，当前为 {:?}",
                new_letter,
                expected_disk_number,
                new_identity
            );
        }

        // 写入标志文件
        let marker_path = format!("{}:\\{}", new_letter, AUTO_CREATED_PARTITION_MARKER);
        std::fs::write(
            &marker_path,
            format!(
                "LetRecovery Auto Created Partition\n\
                Created: {}\n\
                Source: {}:\n\
                SourceDisk: {}\n\
                SourcePartition: {}\n\
                Size: {} MB\n\
                Note: This partition was automatically created and can be safely deleted after system installation.",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                source_letter,
                expected_disk_number,
                expected_partition_number,
                actual_size_mb
            ),
        )
        .map_err(|e| anyhow::anyhow!("{}", tr!("写入标志文件失败: {}", e)))?;

        log::info!(
            "[DISK] 新分区 {}: 创建成功，大小 {} MB，标志文件已写入",
            new_letter,
            actual_size_mb
        );

        Ok(new_letter)
    }

    /// 检查分区是否是自动创建的（通过检查标志文件）
    pub fn is_auto_created_partition(letter: char) -> bool {
        let marker_path = format!("{}:\\{}", letter, AUTO_CREATED_PARTITION_MARKER);
        Path::new(&marker_path).exists()
    }

    /// 删除自动创建的分区
    pub fn delete_auto_created_partition(letter: char) -> Result<()> {
        if !Self::is_auto_created_partition(letter) {
            anyhow::bail!("{}", tr!("分区 {} 不是自动创建的分区", letter));
        }

        log::info!("[DISK] 准备删除自动创建的分区 {}:", letter);

        Self::delete_partition(&letter.to_string())?;
        log::info!("[DISK] WinAPI 已删除自动创建的分区 {}:", letter);

        Ok(())
    }

    /// 查找可用的数据分区（排除指定分区、光驱，检查空间）
    ///
    /// # Arguments
    /// * `exclude_partition` - 要排除的分区（通常是目标安装分区）
    /// * `required_size_bytes` - 需要的最小空间（字节）
    ///
    /// # Returns
    /// * `Ok(Some((partition, is_auto_created)))` - 找到可用分区，返回分区盘符和是否是自动创建的
    /// * `Ok(None)` - 没有找到可用分区，且无法自动创建
    /// * `Err` - 发生错误
    pub fn find_suitable_data_partition(
        exclude_partition: &str,
        image_size_bytes: u64,
    ) -> Result<Option<(String, bool)>> {
        let exclude_letter = exclude_partition
            .chars()
            .next()
            .unwrap_or('C')
            .to_ascii_uppercase();

        log::info!(
            "[DISK] 查找数据分区，目标: {}, 镜像: {} bytes ({:.2} GB)",
            exclude_partition,
            image_size_bytes,
            image_size_bytes as f64 / 1024.0 / 1024.0 / 1024.0
        );

        let fixed_partitions = Self::get_partitions()?;
        let target = fixed_partitions
            .iter()
            .find(|partition| {
                partition
                    .letter
                    .chars()
                    .next()
                    .is_some_and(|letter| letter.eq_ignore_ascii_case(&exclude_letter))
            })
            .cloned();
        let target_disk_number = target.as_ref().and_then(|partition| partition.disk_number);
        let staging_partitions = Self::get_staging_partitions()?;

        let mut profiles = std::collections::HashMap::new();
        let mut candidates = Vec::new();
        for (partition, drive_kind) in &staging_partitions {
            let Some(letter) = partition
                .letter
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
            else {
                continue;
            };
            if letter == exclude_letter || letter == 'X' {
                continue;
            }
            if partition.bitlocker_status != VolumeStatus::NotEncrypted {
                log::warn!(
                    "[DISK] 跳过 {}:：BitLocker 状态为 {}，重启到 PE 后不能保证可访问",
                    letter,
                    partition.bitlocker_status.as_str()
                );
                continue;
            }
            let (media, detected_attachment) = partition
                .disk_number
                .map(|disk_number| {
                    *profiles
                        .entry(disk_number)
                        .or_insert_with(|| Self::get_storage_profile(disk_number))
                })
                .unwrap_or((StorageMedia::Unknown, StorageAttachment::Unknown));
            let attachment = if *drive_kind == StagingDriveKind::Removable {
                StorageAttachment::External
            } else {
                detected_attachment
            };
            let candidate = StagingCandidate {
                letter,
                disk_number: partition.disk_number,
                media,
                attachment,
                free_bytes: partition.free_size_mb.saturating_mul(1024 * 1024),
                total_bytes: partition.total_size_mb.saturating_mul(1024 * 1024),
                is_current_system: partition.is_system_partition,
            };
            log::info!(
                "[DISK] 暂存候选 {}: 磁盘={:?} 介质={:?} 接口={:?} 剩余={:.2} GB",
                letter,
                candidate.disk_number,
                candidate.media,
                candidate.attachment,
                candidate.free_bytes as f64 / 1024.0 / 1024.0 / 1024.0
            );
            candidates.push(candidate);
        }

        let initial_plan =
            select_staging_plan(image_size_bytes, target_disk_number, &candidates, None);
        let initial_plan_uses_external = match initial_plan {
            StagingPlan::Existing { letter, .. } => candidates
                .iter()
                .find(|candidate| candidate.letter.eq_ignore_ascii_case(&letter))
                .is_some_and(|candidate| candidate.attachment == StorageAttachment::External),
            _ => false,
        };
        let should_probe_shrink = matches!(initial_plan, StagingPlan::Unavailable { .. })
            || initial_plan_uses_external
            || (image_size_bytes >= 8 * 1024 * 1024 * 1024
                && target_disk_number
                    .map(|disk_number| {
                        *profiles
                            .entry(disk_number)
                            .or_insert_with(|| Self::get_storage_profile(disk_number))
                    })
                    .is_some_and(|profile| profile.0 == StorageMedia::SolidState));

        let mut max_shrink_mb = None;
        let shrink_candidate = if should_probe_shrink {
            target.as_ref().and_then(|target| {
                let disk_number = target.disk_number?;
                let partition_number = target.partition_number?;
                let (free_bytes, total_bytes) = Self::get_volume_space_bytes(exclude_letter)?;
                let (media, attachment) = *profiles
                    .entry(disk_number)
                    .or_insert_with(|| Self::get_storage_profile(disk_number));
                let file_system = Self::get_volume_file_system(exclude_letter);
                let shrink_is_safe = auto_shrink_target_is_safe(
                    file_system.as_deref(),
                    target.bitlocker_status,
                    attachment,
                    target.is_system_partition,
                );
                if !shrink_is_safe {
                    log::warn!(
                        "[DISK] 不自动缩小 {}:：文件系统={:?} BitLocker={} 接口={:?}",
                        exclude_letter,
                        file_system,
                        target.bitlocker_status.as_str(),
                        attachment
                    );
                    return Some(ShrinkCandidate {
                        letter: exclude_letter,
                        disk_number: Some(disk_number),
                        media,
                        attachment,
                        free_bytes,
                        total_bytes,
                        is_current_system: target.is_system_partition,
                        max_shrink_bytes: 0,
                        shrink_is_safe: false,
                    });
                }

                let queried_mb = match Self::query_shrink_max(exclude_letter) {
                    Ok(value) => value,
                    Err(error) => {
                        log::warn!(
                            "[DISK] 查询 {}: 可缩小空间失败，不执行自动分区: {}",
                            exclude_letter,
                            error
                        );
                        0
                    }
                };
                max_shrink_mb = Some(queried_mb);
                log::info!(
                    "[DISK] 缩卷候选 {}: 磁盘={} 分区={} 最大={} MB 当前空闲={:.2} GB",
                    exclude_letter,
                    disk_number,
                    partition_number,
                    queried_mb,
                    free_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                Some(ShrinkCandidate {
                    letter: exclude_letter,
                    disk_number: Some(disk_number),
                    media,
                    attachment,
                    free_bytes,
                    total_bytes,
                    is_current_system: target.is_system_partition,
                    max_shrink_bytes: queried_mb.saturating_mul(1024 * 1024),
                    shrink_is_safe: true,
                })
            })
        } else {
            None
        };

        match select_staging_plan(
            image_size_bytes,
            target_disk_number,
            &candidates,
            shrink_candidate,
        ) {
            StagingPlan::Existing {
                letter,
                required_bytes,
            } => {
                log::info!(
                    "[DISK] 选择现有数据分区 {}:，安全需求 {:.2} GB",
                    letter,
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                Ok(Some((format!("{}:", letter), false)))
            }
            StagingPlan::ShrinkTarget {
                letter,
                size_mb,
                required_bytes,
            } => {
                let target = target.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("无法取得目标分区 {}: 的稳定身份", exclude_letter)
                })?;
                let disk_number = target
                    .disk_number
                    .ok_or_else(|| anyhow::anyhow!("无法取得目标分区物理磁盘号"))?;
                let partition_number = target
                    .partition_number
                    .ok_or_else(|| anyhow::anyhow!("无法取得目标分区号"))?;
                log::info!(
                    "[DISK] 将从 {}: 安全缩出 {} MB 临时分区，需求 {:.2} GB",
                    letter,
                    size_mb,
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                let new_letter = Self::shrink_and_create_partition_with_marker(
                    letter,
                    size_mb,
                    max_shrink_mb,
                    disk_number,
                    partition_number,
                    target.bitlocker_status,
                    target.is_system_partition,
                )?;
                Ok(Some((format!("{}:", new_letter), true)))
            }
            StagingPlan::Unavailable { required_bytes } => {
                log::error!(
                    "[DISK] 没有满足安全余量的暂存位置，需要 {:.2} GB",
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        auto_shrink_target_is_safe, classify_staging_drive_type, StagingDriveKind, DRIVE_CDROM,
        DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };
    use crate::core::bitlocker::VolumeStatus;
    use lr_core::data_staging::StorageAttachment;

    #[test]
    fn staging_drive_types_exclude_optical_remote_and_ram_disks() {
        assert_eq!(
            classify_staging_drive_type(DRIVE_FIXED),
            Some(StagingDriveKind::Fixed)
        );
        assert_eq!(
            classify_staging_drive_type(DRIVE_REMOVABLE),
            Some(StagingDriveKind::Removable)
        );
        assert_eq!(classify_staging_drive_type(DRIVE_CDROM), None);
        assert_eq!(classify_staging_drive_type(DRIVE_REMOTE), None);
        assert_eq!(classify_staging_drive_type(DRIVE_RAMDISK), None);
    }

    #[test]
    fn stable_unlocked_bitlocker_current_system_volume_can_be_shrunk() {
        assert!(auto_shrink_target_is_safe(
            Some("NTFS"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::Internal,
            true,
        ));
        assert!(auto_shrink_target_is_safe(
            Some("ntfs"),
            VolumeStatus::NotEncrypted,
            StorageAttachment::Internal,
            false,
        ));
    }

    #[test]
    fn unsafe_bitlocker_and_storage_states_remain_fail_closed() {
        for status in [
            VolumeStatus::EncryptedLocked,
            VolumeStatus::Encrypting,
            VolumeStatus::Decrypting,
            VolumeStatus::Unknown,
        ] {
            assert!(!auto_shrink_target_is_safe(
                Some("NTFS"),
                status,
                StorageAttachment::Internal,
                true,
            ));
        }
        assert!(!auto_shrink_target_is_safe(
            Some("NTFS"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::Internal,
            false,
        ));
        assert!(!auto_shrink_target_is_safe(
            Some("NTFS"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::External,
            true,
        ));
        assert!(!auto_shrink_target_is_safe(
            Some("FAT32"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::Internal,
            true,
        ));
    }
}
