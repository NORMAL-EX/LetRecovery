use anyhow::{Context, Result};
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW, FILE_ATTRIBUTE_NORMAL,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    IOCTL_DISK_GET_PARTITION_INFO_EX, IOCTL_STORAGE_GET_DEVICE_NUMBER, PARTITION_INFORMATION_EX,
    STORAGE_DEVICE_NUMBER,
};
use windows::Win32::System::IO::DeviceIoControl;

use crate::tr;

const DRIVE_FIXED: u32 = 3;
#[cfg(test)]
const MAX_ADJACENCY_GAP_BYTES: u64 = 1024 * 1024;

/// 分区表类型
#[derive(Debug, Clone, Copy, PartialEq, Default)]
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
            PartitionStyle::Unknown => write!(f, "未知"),
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
}

/// 分区详细信息
#[derive(Debug, Clone)]
pub struct PartitionDetail {
    pub style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PartitionGeometry {
    disk_number: u32,
    partition_number: u32,
    starting_offset: u64,
    partition_length: u64,
}

#[cfg(test)]
impl PartitionGeometry {
    fn end_offset(self) -> Option<u64> {
        self.starting_offset.checked_add(self.partition_length)
    }
}

#[cfg(test)]
fn partitions_are_physically_adjacent(
    target: PartitionGeometry,
    temporary: PartitionGeometry,
) -> bool {
    if target.disk_number != temporary.disk_number
        || target.partition_number == temporary.partition_number
    {
        return false;
    }
    let Some(target_end) = target.end_offset() else {
        return false;
    };
    temporary
        .starting_offset
        .checked_sub(target_end)
        .is_some_and(|gap| gap <= MAX_ADJACENCY_GAP_BYTES)
}

fn collect_canonical_candidate_snapshots<F>(
    disk_numbers: Result<Vec<u32>>,
    mut snapshot: F,
) -> Result<Vec<(u32, lr_core::windows_storage::DiskLayoutSnapshot)>>
where
    F: FnMut(u32) -> Result<lr_core::windows_storage::DiskLayoutSnapshot>,
{
    disk_numbers?
        .into_iter()
        .map(|disk_number| snapshot(disk_number).map(|layout| (disk_number, layout)))
        .collect()
}

pub struct DiskManager;

impl DiskManager {
    /// Resolve one authenticated canonical extent without trusting a drive letter.  The complete
    /// physical-disk inventory is captured first, cloned layouts remain ambiguous, and the volume
    /// GUID path is derived only after the unique disk has been selected.
    pub fn resolve_canonical_volume_root(
        canonical: &lr_core::install_handoff::CanonicalInstallTargetV2,
    ) -> Result<(lr_core::windows_storage::VolumeIdentity, String)> {
        let disk_numbers = lr_core::windows_storage::physical_disk_numbers()
            .map_err(anyhow::Error::from)
            .context("enumerate physical disks for authenticated handoff volume")?;
        let candidates = collect_canonical_candidate_snapshots(Ok(disk_numbers), |disk_number| {
            lr_core::windows_storage::disk_layout_snapshot(disk_number)
                .map_err(anyhow::Error::from)
                .with_context(|| {
                    format!("capture disk {disk_number} layout for authenticated handoff volume")
                })
        })?;
        let disk_number =
            lr_core::install_handoff::unique_canonical_target_match(canonical, &candidates)?;
        let extent = lr_core::windows_storage::VolumeIdentity {
            disk_number,
            offset_bytes: canonical.partition_offset_bytes,
            extent_length_bytes: canonical.partition_length_bytes,
        };
        let root = lr_core::windows_storage::volume_guid_path_for_partition(
            disk_number,
            canonical.partition_offset_bytes,
        )
        .map_err(anyhow::Error::from)
        .context("resolve authenticated handoff extent to a volume GUID path")?;

        // Re-capture after VDS/volume resolution.  A topology change between inventory and volume
        // lookup must not turn the selected disk number into an authorization by itself.
        let rebound = lr_core::windows_storage::disk_layout_snapshot(disk_number)
            .map_err(anyhow::Error::from)
            .context("re-capture authenticated handoff disk after volume GUID resolution")?;
        if !canonical.matches_snapshot(&rebound) {
            anyhow::bail!(
                "authenticated handoff disk layout changed while resolving its volume GUID path"
            );
        }
        Ok((extent, root))
    }

    /// Resolve the current unique drive letter for a canonical extent.  Drive letters are output
    /// aliases only: authorization is established by `resolve_canonical_volume_root`, and every
    /// candidate letter is compared with that exact extent before it is returned.
    pub fn resolve_canonical_drive_letter(
        canonical: &lr_core::install_handoff::CanonicalInstallTargetV2,
    ) -> Result<char> {
        let (expected, _) = Self::resolve_canonical_volume_root(canonical)?;
        let matches = (b'A'..=b'Z')
            .filter_map(|value| {
                let letter = value as char;
                lr_core::windows_storage::volume_identity(letter)
                    .ok()
                    .filter(|actual| {
                        lr_core::windows_storage::same_volume_identity(*actual, expected)
                    })
                    .map(|_| letter)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [letter] => Ok(*letter),
            [] => anyhow::bail!("canonical handoff extent has no current drive-letter access path"),
            _ => anyhow::bail!(
                "canonical handoff extent has multiple drive-letter access paths: {:?}",
                matches
            ),
        }
    }

    /// Delete only the manifest-authenticated temporary extent and return the complete
    /// provider-reclaimed tail (including legal free gaps on either side of the temporary
    /// partition) to the authenticated source's exact pre-shrink boundary.
    /// No marker or drive-letter scan participates in
    /// authorization; both full canonical layouts are rechecked immediately before the checked
    /// topology mutation.
    pub fn cleanup_authenticated_auto_staging(
        authorization: &lr_core::handoff_manifest::AutoStagingAuthorization,
    ) -> Result<lr_core::windows_storage::VolumeIdentity> {
        let (source_extent, _) = Self::resolve_canonical_volume_root(&authorization.source)?;
        let (temporary_extent, _) = Self::resolve_canonical_volume_root(&authorization.temporary)?;
        if source_extent.disk_number != temporary_extent.disk_number {
            anyhow::bail!("authenticated auto-staging extents are on different disks");
        }
        let source_end = source_extent
            .offset_bytes
            .checked_add(source_extent.extent_length_bytes)
            .ok_or_else(|| anyhow::anyhow!("authenticated source extent end overflows"))?;
        if source_end > temporary_extent.offset_bytes {
            anyhow::bail!("authenticated temporary extent overlaps the source extent");
        }
        let layout = lr_core::windows_storage::disk_layout_snapshot(source_extent.disk_number)?;
        if !authorization.source.matches_snapshot(&layout)
            || !authorization.temporary.matches_snapshot(&layout)
        {
            anyhow::bail!("disk layout changed before authenticated staging cleanup");
        }
        let reclaim_length = authorization.reclaim_length_bytes()?;
        let original_source_end = source_extent
            .offset_bytes
            .checked_add(authorization.source_length_before_bytes)
            .ok_or_else(|| anyhow::anyhow!("authenticated original source extent end overflows"))?;
        let temporary_end = temporary_extent
            .offset_bytes
            .checked_add(temporary_extent.extent_length_bytes)
            .ok_or_else(|| anyhow::anyhow!("authenticated temporary extent end overflows"))?;
        let current_free =
            lr_core::windows_storage::current_free_extents(source_extent.disk_number)?;
        let range_is_free = |start: u64, end: u64| {
            start == end
                || current_free.iter().any(|extent| {
                    extent.offset_bytes <= start
                        && extent
                            .offset_bytes
                            .checked_add(extent.length_bytes)
                            .is_some_and(|free_end| free_end >= end)
                })
        };
        let gap_length = temporary_extent
            .offset_bytes
            .checked_sub(source_end)
            .ok_or_else(|| anyhow::anyhow!("authenticated staging gap underflows"))?;
        if gap_length != 0 && !range_is_free(source_end, temporary_extent.offset_bytes) {
            anyhow::bail!(
                "provider no longer reports the authenticated gap before staging as free"
            );
        }
        if !range_is_free(temporary_end, original_source_end) {
            anyhow::bail!("provider no longer reports the authenticated gap after staging as free");
        }
        let target_letter = Self::resolve_canonical_drive_letter(&authorization.source)?;
        let target_identity = lr_core::windows_storage::stable_volume_identity(target_letter)?;
        if !lr_core::windows_storage::same_volume_identity(target_identity.extent, source_extent) {
            anyhow::bail!("authenticated source volume changed before staging cleanup");
        }
        lr_core::windows_storage::delete_partition_checked(
            temporary_extent.disk_number,
            temporary_extent.offset_bytes,
            true,
            &layout,
        )?;
        lr_core::windows_storage::extend_volume_stable_checked(
            target_letter,
            target_identity,
            reclaim_length,
        )?;
        let expected_length = authorization.source_length_before_bytes;
        let actual = lr_core::windows_storage::volume_identity(target_letter)?;
        if actual.disk_number != source_extent.disk_number
            || actual.offset_bytes != source_extent.offset_bytes
            || actual.extent_length_bytes != expected_length
        {
            anyhow::bail!(
                "temporary extent was removed but final source extent readback is inconsistent"
            );
        }
        Ok(actual)
    }

    pub fn partition_volume_identity(
        partition: &str,
    ) -> Result<lr_core::windows_storage::VolumeIdentity> {
        let spec = lr_core::format_command::FormatCommandSpec::new(partition, "NTFS", None)
            .map_err(|error| anyhow::anyhow!("无效的目标分区: {error}"))?;
        let drive_letter = spec.drive().as_bytes()[0] as char;
        lr_core::windows_storage::volume_identity(drive_letter).map_err(anyhow::Error::from)
    }

    /// Re-resolve a drive letter immediately before a destructive install phase.
    ///
    /// Only the physical disk extent is compared. Formatting legitimately changes the label,
    /// file system and free-space snapshot, none of which may invalidate the authorization.
    pub fn verify_partition_volume_identity(
        partition: &str,
        expected: lr_core::windows_storage::VolumeIdentity,
    ) -> Result<()> {
        let actual = Self::partition_volume_identity(partition)?;
        if !lr_core::windows_storage::same_volume_identity(actual, expected) {
            anyhow::bail!(
                "target {} now maps to disk {} offset {} length {}; expected disk {} offset {} length {}",
                partition,
                actual.disk_number,
                actual.offset_bytes,
                actual.extent_length_bytes,
                expected.disk_number,
                expected.offset_bytes,
                expected.extent_length_bytes
            );
        }
        Ok(())
    }

    fn partition_geometry(drive: &str) -> Result<PartitionGeometry> {
        let letter = drive
            .chars()
            .next()
            .filter(|character| character.is_ascii_alphabetic())
            .ok_or_else(|| anyhow::anyhow!("无效的分区盘符: {drive}"))?
            .to_ascii_uppercase();
        let device_path = format!(r"\\.\{}:", letter);
        let wide_device_path: Vec<u16> = device_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide_device_path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                HANDLE::default(),
            )
        }
        .map_err(|error| anyhow::anyhow!("打开卷 {letter}: 查询分区身份失败: {error}"))?;

        let result = (|| {
            let mut device_number = STORAGE_DEVICE_NUMBER::default();
            let mut bytes_returned = 0u32;
            unsafe {
                DeviceIoControl(
                    handle,
                    IOCTL_STORAGE_GET_DEVICE_NUMBER,
                    None,
                    0,
                    Some((&mut device_number as *mut STORAGE_DEVICE_NUMBER).cast()),
                    std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
                    Some(&mut bytes_returned),
                    None,
                )
            }
            .map_err(|error| anyhow::anyhow!("查询卷 {letter}: 的磁盘号和分区号失败: {error}"))?;
            if bytes_returned < std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32 {
                anyhow::bail!("查询卷 {letter}: 的磁盘身份返回数据不完整");
            }

            let mut partition_info = PARTITION_INFORMATION_EX::default();
            bytes_returned = 0;
            unsafe {
                DeviceIoControl(
                    handle,
                    IOCTL_DISK_GET_PARTITION_INFO_EX,
                    None,
                    0,
                    Some((&mut partition_info as *mut PARTITION_INFORMATION_EX).cast()),
                    std::mem::size_of::<PARTITION_INFORMATION_EX>() as u32,
                    Some(&mut bytes_returned),
                    None,
                )
            }
            .map_err(|error| anyhow::anyhow!("查询卷 {letter}: 的分区几何信息失败: {error}"))?;
            if bytes_returned < std::mem::size_of::<PARTITION_INFORMATION_EX>() as u32 {
                anyhow::bail!("查询卷 {letter}: 的分区几何信息返回数据不完整");
            }
            if partition_info.StartingOffset < 0 || partition_info.PartitionLength <= 0 {
                anyhow::bail!("卷 {letter}: 返回了无效的分区几何信息");
            }
            if device_number.PartitionNumber != partition_info.PartitionNumber {
                anyhow::bail!(
                    "卷 {letter}: 的分区身份不一致（storage={}，disk={}）",
                    device_number.PartitionNumber,
                    partition_info.PartitionNumber
                );
            }

            Ok(PartitionGeometry {
                disk_number: device_number.DeviceNumber,
                partition_number: partition_info.PartitionNumber,
                starting_offset: partition_info.StartingOffset as u64,
                partition_length: partition_info.PartitionLength as u64,
            })
        })();

        if let Err(error) = unsafe { CloseHandle(handle) } {
            log::warn!("关闭卷 {}: 查询句柄失败: {}", letter, error);
        }
        result
    }

    fn format_failure_hint() -> String {
        tr!(
            "可能原因:\n- 目标盘质量较差或已损坏（坏盘/扩容盘/掉盘）\n- 磁盘存在坏道、I/O 错误或 CRC 错误\n- 数据线、USB 口、硬盘盒或供电不稳定\n- 分区被占用、写保护或分区表异常"
        )
    }

    /// 获取所有固定磁盘分区列表
    pub fn get_partitions() -> Result<Vec<Partition>> {
        let mut partitions = Vec::new();
        let running_windows_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;

        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            if let Ok(info) = Self::get_partition_info(&drive, running_windows_drive) {
                log::debug!(
                    "Partition {} label=\"{}\" total={}MB free={}MB system={} windows={} style={}",
                    info.letter.as_str(),
                    info.label.as_str(),
                    info.total_size_mb,
                    info.free_size_mb,
                    info.is_system_partition,
                    info.has_windows,
                    info.partition_style
                );
                partitions.push(info);
            }
        }

        Ok(partitions)
    }

    fn get_partition_info(drive: &str, running_windows_drive: char) -> Result<Partition> {
        let path = format!("{}\\", drive);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        // 获取驱动器类型
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };
        if drive_type != DRIVE_FIXED {
            anyhow::bail!("Not a fixed drive");
        }

        // 获取磁盘空间
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

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

        let is_current_system = drive
            .chars()
            .next()
            .is_some_and(|letter| letter.eq_ignore_ascii_case(&running_windows_drive));

        // 检查是否包含 Windows 系统
        let windows_path = format!("{}\\Windows\\System32", drive);
        let has_windows = Path::new(&windows_path).exists();

        // PE环境下，is_system_partition 表示是否包含 Windows（排除PE自己的X盘）
        let is_system_partition = has_windows && !is_current_system;

        // 获取分区表类型、磁盘号和分区号
        let detail = Self::get_partition_style(drive);

        Ok(Partition {
            letter: drive.to_string(),
            total_size_mb: total_bytes / 1024 / 1024,
            free_size_mb: free_bytes_available / 1024 / 1024,
            label,
            is_system_partition,
            has_windows,
            partition_style: detail.style,
            disk_number: detail.disk_number,
            partition_number: detail.partition_number,
        })
    }

    /// 获取分区表类型和分区号 (GPT/MBR)
    fn get_partition_style(drive: &str) -> PartitionDetail {
        match Self::partition_geometry(drive) {
            Ok(geometry) => PartitionDetail {
                style: Self::get_disk_partition_style(geometry.disk_number),
                disk_number: Some(geometry.disk_number),
                partition_number: Some(geometry.partition_number),
            },
            Err(error) => {
                log::warn!("[disk] Win32 分区身份查询失败: {}", error);
                PartitionDetail {
                    style: PartitionStyle::Unknown,
                    disk_number: None,
                    partition_number: None,
                }
            }
        }
    }

    /// 获取指定磁盘的分区表类型
    fn get_disk_partition_style(disk_number: u32) -> PartitionStyle {
        match lr_core::windows_storage::disk_style(disk_number) {
            Ok(lr_core::windows_storage::DiskStyle::Gpt) => PartitionStyle::GPT,
            Ok(lr_core::windows_storage::DiskStyle::Mbr) => PartitionStyle::MBR,
            Err(error) => {
                log::debug!(
                    "[disk] 无法查询磁盘 {} 的分区表类型: {}",
                    disk_number,
                    error
                );
                PartitionStyle::Unknown
            }
        }
    }

    /// Proves that the staged source and running WinPE cannot be overwritten by target writes.
    /// This is required even when the caller keeps the existing file system.
    pub fn validate_install_target_dependencies(
        partition: &str,
        expected_target: lr_core::windows_storage::VolumeIdentity,
        source_path: &Path,
    ) -> Result<()> {
        let spec = lr_core::format_command::FormatCommandSpec::new(partition, "NTFS", None)
            .map_err(|error| anyhow::anyhow!("无效的目标分区参数: {error}"))?;
        let drive = spec.drive().to_string();
        let drive_letter = drive.as_bytes()[0] as char;
        let source = std::fs::canonicalize(source_path).with_context(|| {
            format!(
                "resolve install source before target write: {}",
                source_path.display()
            )
        })?;
        let source_letter = lr_core::windows_storage::path_drive_letter(&source)
            .or_else(|| lr_core::windows_storage::path_drive_letter(source_path))
            .ok_or_else(|| anyhow::anyhow!("安装镜像没有可验证的本地盘符，已停止写入"))?;
        if source_letter.eq_ignore_ascii_case(&drive_letter) {
            anyhow::bail!(
                "{}",
                tr!(
                    "安装镜像位于目标分区 {}，为防止覆盖自身输入已停止写入。",
                    drive
                )
            );
        }
        match lr_core::windows_storage::volume_identity(source_letter) {
            Ok(source_identity)
                if lr_core::windows_storage::same_physical_partition(
                    expected_target,
                    source_identity,
                ) =>
            {
                anyhow::bail!(
                    "{}",
                    tr!("安装镜像与目标分区 {} 指向同一物理卷，已停止写入。", drive)
                );
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    lr_core::windows_storage::drive_kind(source_letter),
                    Ok(lr_core::windows_storage::DriveKind::Optical)
                        | Ok(lr_core::windows_storage::DriveKind::RamDisk)
                ) =>
            {
                log::debug!(
                    "安装源位于只读光驱或 WinPE RAM disk {}:，物理范围不可查询且盘符与目标不同: {}",
                    source_letter,
                    error
                );
            }
            Err(error) => return Err(anyhow::Error::from(error)),
        }
        let running_windows = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;
        if running_windows.eq_ignore_ascii_case(&drive_letter) {
            anyhow::bail!("{}", tr!("不能写入当前运行的 WinPE 卷 {}。", drive));
        }
        match lr_core::windows_storage::volume_identity(running_windows) {
            Ok(running_identity)
                if running_identity.disk_number == expected_target.disk_number
                    && running_identity.offset_bytes == expected_target.offset_bytes =>
            {
                anyhow::bail!(
                    "{}",
                    tr!("目标分区 {} 与当前运行的 WinPE 指向同一物理卷。", drive)
                );
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    lr_core::windows_storage::drive_kind(running_windows),
                    Ok(lr_core::windows_storage::DriveKind::RamDisk)
                ) =>
            {
                log::debug!(
                    "当前 WinPE 位于 RAM disk {}:，无法查询本地磁盘范围且与目标盘符不同: {}",
                    running_windows,
                    error
                );
            }
            Err(error) => return Err(anyhow::Error::from(error)),
        }
        Ok(())
    }

    /// 格式化指定分区（带卷标）
    ///
    pub fn format_partition_with_label(
        partition: &str,
        expected_target: lr_core::windows_storage::VolumeIdentity,
        volume_label: Option<&str>,
    ) -> Result<String> {
        log::info!("格式化分区: {} 卷标: {:?}", partition, volume_label);
        let vol_label = match volume_label {
            Some(label) if !label.is_empty() => label,
            _ => "本地磁盘",
        };
        let spec =
            lr_core::format_command::FormatCommandSpec::new(partition, "NTFS", Some(vol_label))
                .map_err(|error| anyhow::anyhow!("无效的格式化参数: {error}"))?;
        let drive = spec.drive().to_string();
        let drive_letter = drive.as_bytes()[0] as char;
        let vol_label = spec.volume_label().unwrap_or("本地磁盘");
        lr_core::windows_storage::format_drive_with_options_checked(
            drive_letter,
            expected_target,
            &lr_core::windows_storage::FormatOptions {
                file_system: lr_core::windows_storage::FileSystem::Ntfs,
                label: vol_label.to_owned(),
                allocation_unit_size: 0,
                quick: true,
                // WinPE commonly leaves read-only probe handles on an offline Windows volume.
                // The source/current-PE guards above make a forced VDS dismount safe here.
                force_dismount: true,
            },
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "{}\n{}",
                tr!("格式化分区失败: {}", error),
                Self::format_failure_hint()
            )
        })?;
        log::info!("分区 {} 已通过 VDS 格式化并核验", drive);
        Ok(tr!("分区 {} 格式化成功", drive))
    }

    /// 检测是否为UEFI模式
    pub fn detect_uefi_mode() -> anyhow::Result<bool> {
        match lr_core::windows_firmware::detect_firmware_type()? {
            lr_core::windows_firmware::FirmwareType::Uefi => Ok(true),
            lr_core::windows_firmware::FirmwareType::Bios => Ok(false),
        }
    }

    /// Resolve the install boot mode against the selected target disk.
    ///
    /// Auto must follow the target partition table rather than the way WinPE
    /// itself was booted. The PE firmware mode is only a last-resort fallback
    /// when the Win32 storage provider cannot identify the target disk layout.
    pub fn resolve_install_uefi_mode(
        boot_mode: u8,
        target_partition: &str,
    ) -> anyhow::Result<bool> {
        match boot_mode {
            1 => return Ok(true),
            2 => return Ok(false),
            _ => {}
        }
        let detail = Self::get_partition_style(target_partition);
        Self::resolve_install_uefi_mode_with(boot_mode, detail.style, Self::detect_uefi_mode)
    }

    fn resolve_install_uefi_mode_with<F>(
        boot_mode: u8,
        target_style: PartitionStyle,
        detect_current_firmware: F,
    ) -> anyhow::Result<bool>
    where
        F: FnOnce() -> anyhow::Result<bool>,
    {
        match boot_mode {
            1 => Ok(true),
            2 => Ok(false),
            _ => match target_style {
                PartitionStyle::GPT => {
                    log::info!("[BOOT] 自动模式：目标分区位于 GPT 磁盘，使用 UEFI");
                    Ok(true)
                }
                PartitionStyle::MBR => {
                    log::info!("[BOOT] 自动模式：目标分区位于 MBR 磁盘，使用 Legacy");
                    Ok(false)
                }
                PartitionStyle::Unknown => {
                    let fallback = detect_current_firmware()?;
                    log::warn!(
                        "[BOOT] 无法识别目标分区的分区表，回退当前 PE 固件模式: {}",
                        if fallback { "UEFI" } else { "Legacy" }
                    );
                    Ok(fallback)
                }
            },
        }
    }

    /// 获取分区大小（MB）
    fn get_partition_size_mb(letter: char) -> Option<u64> {
        let path = format!("{}:\\", letter);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let mut total_bytes: u64 = 0;

        unsafe {
            let result = GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                None,
                Some(&mut total_bytes as *mut u64),
                None,
            );

            if result.is_ok() {
                Some(total_bytes / 1024 / 1024)
            } else {
                None
            }
        }
    }

    /// 无损扩大分区到指定大小（仅并入紧邻其后的未分配空间；不移动其它分区）。
    ///
    /// - `letter`：目标分区盘符（如 'C'）。在 PE 下应由扩容标记定位后传入。
    /// - `target_size_mb`：期望最终总大小（MB）；0 = 尽可能扩到最大（吃光相邻未分配空间）。
    ///
    /// 实现：VDS 只并入紧跟该卷的连续未分配空间；若其后是别的分区则失败关闭。
    pub fn expand_partition_lossless_checked(
        letter: char,
        target_size_mb: u64,
        expected: lr_core::windows_storage::VolumeIdentity,
    ) -> Result<String> {
        let current_mb = Self::get_partition_size_mb(letter)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法获取分区 {}: 的当前大小", letter)))?;
        log::info!(
            "[EXPAND] 目标分区 {}: 当前 {} MB，目标 {} MB",
            letter,
            current_mb,
            target_size_mb
        );

        let identity = lr_core::windows_storage::stable_volume_identity(letter)?;
        if !lr_core::windows_storage::same_volume_identity(identity.extent, expected) {
            anyhow::bail!(
                "authenticated expand target {}: changed before VDS extend: actual disk={} offset={} length={}, expected disk={} offset={} length={}",
                letter,
                identity.extent.disk_number,
                identity.extent.offset_bytes,
                identity.extent.extent_length_bytes,
                expected.disk_number,
                expected.offset_bytes,
                expected.extent_length_bytes
            );
        }
        let available = lr_core::windows_storage::contiguous_free_bytes_after(
            identity.extent.disk_number,
            identity
                .extent
                .offset_bytes
                .checked_add(identity.extent.extent_length_bytes)
                .ok_or_else(|| anyhow::anyhow!("分区末端偏移计算溢出"))?,
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "{}",
                tr!("目标分区后面没有相邻的未分配空间可并入。若要从后面的分区夺取空间，需要分区移动功能。")
            )
        })?;
        let bytes_to_add = if target_size_mb == 0 || target_size_mb <= current_mb {
            available
        } else {
            (target_size_mb - current_mb)
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("目标分区大小超出支持范围"))?
        };
        if bytes_to_add == 0 || bytes_to_add > available {
            anyhow::bail!(
                "{}",
                tr!("目标分区后面的连续未分配空间不足。若要从后面的分区夺取空间，需要分区移动功能。")
            );
        }
        Self::verify_partition_volume_identity(&format!("{}:", letter), expected)?;
        lr_core::windows_storage::extend_volume_stable_checked(letter, identity, bytes_to_add)?;
        let new_mb = Self::get_partition_size_mb(letter).unwrap_or(current_mb);
        let expected_mb = (identity.extent.extent_length_bytes + bytes_to_add) / 1024 / 1024;
        if new_mb != expected_mb {
            anyhow::bail!(
                "{}",
                tr!(
                    "扩容操作返回成功，但大小核验失败：期望 {} MB，实际 {} MB。",
                    expected_mb,
                    new_mb,
                )
            );
        }
        Ok(tr!(
            "分区 {}: 已从 {} MB 扩大到 {} MB",
            letter,
            current_mb,
            new_mb
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_canonical_candidate_snapshots, partitions_are_physically_adjacent, DiskManager,
        PartitionGeometry, PartitionStyle,
    };

    fn canonical_snapshot() -> lr_core::windows_storage::DiskLayoutSnapshot {
        lr_core::windows_storage::DiskLayoutSnapshot {
            disk_size_bytes: 10_000_000,
            disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [1; 16] },
            device_id_hash: Some([2; 32]),
            partitions: vec![lr_core::windows_storage::DiskLayoutPartitionSnapshot {
                offset_bytes: 1_048_576,
                size_bytes: 5_000_000,
                token: lr_core::windows_storage::DiskLayoutPartitionToken::Gpt {
                    partition_type: [3; 16],
                    partition_id: [4; 16],
                    attributes: 0,
                },
            }],
        }
    }

    #[test]
    fn canonical_inventory_failure_propagates_without_collecting_partial_candidates() {
        let mut snapshot_called = false;
        let result = collect_canonical_candidate_snapshots(
            Err(anyhow::anyhow!("modeled complete inventory failure")),
            |_| {
                snapshot_called = true;
                Ok(canonical_snapshot())
            },
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("complete inventory"));
        assert!(!snapshot_called);
    }

    #[test]
    fn canonical_inventory_rejects_two_matching_cloned_disks() {
        let snapshot = canonical_snapshot();
        let target = lr_core::install_handoff::CanonicalInstallTargetV2::from_snapshot(
            &snapshot, 1_048_576, 5_000_000,
        )
        .unwrap();
        let candidates =
            collect_canonical_candidate_snapshots(Ok(vec![2, 8]), |_| Ok(snapshot.clone()))
                .unwrap();
        assert!(
            lr_core::install_handoff::unique_canonical_target_match(&target, &candidates)
                .unwrap_err()
                .to_string()
                .contains("multiple cloned")
        );
    }

    fn geometry(
        disk_number: u32,
        partition_number: u32,
        starting_offset: u64,
        partition_length: u64,
    ) -> PartitionGeometry {
        PartitionGeometry {
            disk_number,
            partition_number,
            starting_offset,
            partition_length,
        }
    }

    #[test]
    fn validates_physical_partition_adjacency_from_offsets() {
        let target = geometry(0, 2, 1024 * 1024, 40 * 1024 * 1024);
        assert!(partitions_are_physically_adjacent(
            target,
            geometry(0, 3, target.end_offset().unwrap(), 10 * 1024 * 1024)
        ));
        assert!(partitions_are_physically_adjacent(
            target,
            geometry(
                0,
                7,
                target.end_offset().unwrap() + 1024 * 1024,
                10 * 1024 * 1024
            )
        ));
        assert!(!partitions_are_physically_adjacent(
            target,
            geometry(
                0,
                3,
                target.end_offset().unwrap() + 2 * 1024 * 1024,
                10 * 1024 * 1024
            )
        ));
        assert!(!partitions_are_physically_adjacent(
            target,
            geometry(1, 3, target.end_offset().unwrap(), 10 * 1024 * 1024)
        ));
        assert!(!partitions_are_physically_adjacent(
            geometry(0, 2, u64::MAX - 10, 20),
            geometry(0, 3, u64::MAX, 1)
        ));
    }

    #[test]
    fn explicit_boot_mode_is_preserved_and_auto_follows_target_style() {
        let detector_must_not_run = || -> anyhow::Result<bool> {
            panic!("explicit or known target mode must not query current firmware")
        };
        assert!(DiskManager::resolve_install_uefi_mode_with(
            1,
            PartitionStyle::MBR,
            detector_must_not_run,
        )
        .unwrap());
        assert!(!DiskManager::resolve_install_uefi_mode_with(
            2,
            PartitionStyle::GPT,
            detector_must_not_run,
        )
        .unwrap());
        assert!(DiskManager::resolve_install_uefi_mode_with(
            0,
            PartitionStyle::GPT,
            detector_must_not_run,
        )
        .unwrap());
        assert!(!DiskManager::resolve_install_uefi_mode_with(
            0,
            PartitionStyle::MBR,
            detector_must_not_run,
        )
        .unwrap());
    }

    #[test]
    fn unknown_auto_style_uses_firmware_probe_and_propagates_failure() {
        assert!(
            DiskManager::resolve_install_uefi_mode_with(0, PartitionStyle::Unknown, || Ok(true),)
                .unwrap()
        );
        assert!(DiskManager::resolve_install_uefi_mode_with(
            0,
            PartitionStyle::Unknown,
            || anyhow::bail!("probe failed"),
        )
        .is_err());
    }
}
