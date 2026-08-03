use anyhow::Result;
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
const MAX_ADJACENCY_GAP_BYTES: u64 = 1024 * 1024;

/// 自动创建分区的标志文件名
pub const AUTO_CREATED_PARTITION_MARKER: &str = "LetRecovery_AutoCreated.marker";

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

impl PartitionGeometry {
    fn end_offset(self) -> Option<u64> {
        self.starting_offset.checked_add(self.partition_length)
    }
}

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

pub struct DiskManager;

impl DiskManager {
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

    /// 格式化指定分区（带卷标）
    ///
    pub fn format_partition_with_label(
        partition: &str,
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
        lr_core::windows_storage::format_drive(
            drive_letter,
            lr_core::windows_storage::FileSystem::Ntfs,
            vol_label,
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

    fn read_auto_marker_source(letter: char) -> Option<char> {
        let marker_path = format!("{}:\\{}", letter, AUTO_CREATED_PARTITION_MARKER);
        let content = std::fs::read_to_string(marker_path).ok()?;
        for line in content.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("Source:") {
                return value
                    .trim()
                    .chars()
                    .find(|c| c.is_ascii_alphabetic())
                    .map(|c| c.to_ascii_uppercase());
            }
            if let Some(value) = line.strip_prefix("Source=") {
                return value
                    .trim()
                    .chars()
                    .find(|c| c.is_ascii_alphabetic())
                    .map(|c| c.to_ascii_uppercase());
            }
        }
        None
    }

    /// 查找自动创建的分区（通过标志文件）
    /// 返回 (盘符, 磁盘号Option, 来源盘符Option) 如果找到的话
    pub fn find_auto_created_partition() -> Option<(char, Option<u32>, Option<char>)> {
        for letter in b'A'..=b'Z' {
            let c = letter as char;
            // 跳过 X 盘（PE系统盘）
            if c == 'X' {
                continue;
            }

            let marker_path = format!("{}:\\{}", c, AUTO_CREATED_PARTITION_MARKER);
            if Path::new(&marker_path).exists() {
                log::info!("找到自动创建的分区: {}:", c);

                // 获取该分区所在的磁盘号
                let detail = Self::get_partition_style(&format!("{}:", c));
                return Some((c, detail.disk_number, Self::read_auto_marker_source(c)));
            }
        }
        None
    }

    /// 删除自动创建的分区并扩展目标分区
    ///
    /// # Arguments
    /// * `target_partition` - 目标安装分区（如 "D:"），删除数据分区后要扩展的分区
    ///
    /// 流程：
    /// 1. 找到自动创建的分区
    /// 2. 确认该分区和目标分区在同一个磁盘上
    /// 3. 通过 Win32 分区偏移和长度确认临时分区位于目标分区紧邻后方
    /// 4. 记录目标分区当前大小
    /// 5. 删除该分区
    /// 6. 刷新磁盘信息
    /// 7. 扩展目标分区以使用释放的空间
    /// 8. 验证分区大小是否增加
    pub fn cleanup_auto_created_partition_and_extend(target_partition: &str) -> Result<()> {
        let target_letter = target_partition
            .chars()
            .next()
            .unwrap_or('C')
            .to_ascii_uppercase();

        log::info!("[CLEANUP] ========================================");
        log::info!("[CLEANUP] 开始清理自动创建的分区");
        log::info!("[CLEANUP] 目标安装分区: {}:", target_letter);
        log::info!("[CLEANUP] ========================================");

        // 查找自动创建的分区
        let (auto_letter, _auto_disk_num_opt, marker_source) =
            match Self::find_auto_created_partition() {
                Some(info) => info,
                None => {
                    log::info!("[CLEANUP] 未找到自动创建的分区，无需清理");
                    return Ok(());
                }
            };

        let auto_geometry =
            Self::partition_geometry(&format!("{}:", auto_letter)).map_err(|error| {
                anyhow::anyhow!(
                    "[CLEANUP] 无法可靠获取自动创建分区 {}: 的身份和几何信息，已取消删除: {}",
                    auto_letter,
                    error
                )
            })?;

        log::info!(
            "[CLEANUP] 找到自动创建的分区: {}:, 磁盘 {}, 分区号 {:?}",
            auto_letter,
            auto_geometry.disk_number,
            auto_geometry.partition_number
        );

        let target_geometry =
            Self::partition_geometry(&format!("{}:", target_letter)).map_err(|error| {
                anyhow::anyhow!(
                    "[CLEANUP] 无法可靠获取目标分区 {}: 的身份和几何信息，已取消删除: {}",
                    target_letter,
                    error
                )
            })?;

        log::info!(
            "[CLEANUP] 目标分区: {}:, 磁盘 {}, 分区号 {:?}",
            target_letter,
            target_geometry.disk_number,
            target_geometry.partition_number
        );

        let source_letter = marker_source.ok_or_else(|| {
            anyhow::anyhow!(
                "[CLEANUP] 自动创建分区 {} 的标记缺少 Source，无法确认归属，已取消删除",
                auto_letter
            )
        })?;
        if source_letter != target_letter {
            anyhow::bail!(
                "[CLEANUP] 自动创建分区 {} 的 Source 为 {}:，与目标分区 {}: 不一致，已取消删除",
                auto_letter,
                source_letter,
                target_letter
            );
        }

        // 检查是否在同一磁盘
        if auto_geometry.disk_number != target_geometry.disk_number {
            anyhow::bail!(
                "[CLEANUP] 自动创建的分区 (磁盘{}) 和目标分区 (磁盘{}) 不在同一磁盘，已取消删除",
                auto_geometry.disk_number,
                target_geometry.disk_number
            );
        }

        // 基础卷扩展只能使用目标分区物理末端之后的相邻未分配空间。分区号不能证明
        // 物理相邻，因此必须使用 IOCTL 返回的起始偏移和长度做 fail-closed 检查。
        if !partitions_are_physically_adjacent(target_geometry, auto_geometry) {
            anyhow::bail!(
                "[CLEANUP] 自动创建分区 {}: 不是目标分区 {}: 的物理紧邻后方分区，已取消删除",
                auto_letter,
                target_letter
            );
        }
        log::info!(
            "[CLEANUP] 物理相邻性检查通过：目标分区{} (末端={:?}) -> 临时分区{} (起点={})",
            target_geometry.partition_number,
            target_geometry.end_offset(),
            auto_geometry.partition_number,
            auto_geometry.starting_offset
        );

        // 删除自动创建分区并扩展目标分区
        log::info!(
            "[CLEANUP] 开始删除分区 {} 并扩展目标分区 {}...",
            auto_letter,
            target_letter
        );
        Self::delete_partition_and_extend(
            auto_letter,
            target_letter,
            auto_geometry,
            target_geometry,
        )
    }

    /// 删除指定盘符的分区
    #[allow(
        dead_code,
        reason = "retained as a compatibility fallback for PE cleanup flows"
    )]
    fn delete_partition_by_letter(letter: char) -> Result<()> {
        log::info!("[CLEANUP] 删除分区 {}:", letter);
        let identity = lr_core::windows_storage::volume_identity(letter)?;
        lr_core::windows_storage::delete_partition(
            identity.disk_number,
            identity.offset_bytes,
            true,
        )?;
        log::info!("[CLEANUP] 分区 {} 删除成功", letter);
        Ok(())
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

    /// 删除分区并扩展目标分区
    fn delete_partition_and_extend(
        auto_letter: char,
        target_letter: char,
        expected_auto: PartitionGeometry,
        expected_target: PartitionGeometry,
    ) -> Result<()> {
        // 在不可逆删除前重新打开两个卷并比对稳定身份和几何信息，防止扫描后盘符变化、
        // 磁盘插拔或分区表被其它进程修改而删错目标。
        let current_auto = Self::partition_geometry(&format!("{}:", auto_letter))?;
        let current_target = Self::partition_geometry(&format!("{}:", target_letter))?;
        if current_auto != expected_auto || current_target != expected_target {
            anyhow::bail!("[CLEANUP] 删除前分区身份或几何信息发生变化，已取消删除");
        }

        // 记录扩展前的分区大小
        let size_before = current_target.partition_length;
        log::info!(
            "[CLEANUP] 扩展前目标分区大小: {} MB",
            size_before / 1024 / 1024
        );
        log::info!("[CLEANUP] Step 1: 删除分区 {}:", auto_letter);
        lr_core::windows_storage::delete_partition(
            expected_auto.disk_number,
            expected_auto.starting_offset,
            true,
        )?;
        log::info!("[CLEANUP] 分区 {} 删除成功", auto_letter);

        log::info!("[CLEANUP] Step 2: 扩展目标分区 {}", target_letter);
        lr_core::windows_storage::extend_volume(
            target_letter,
            expected_target.disk_number,
            expected_auto.partition_length,
        )
        .map_err(|error| anyhow::anyhow!("临时分区已删除，但扩展目标分区失败: {error}"))?;
        let current = Self::partition_geometry(&format!("{}:", target_letter))?;
        let expected_size = size_before
            .checked_add(expected_auto.partition_length)
            .ok_or_else(|| anyhow::anyhow!("扩展后的分区大小计算溢出"))?;
        if current.disk_number != expected_target.disk_number
            || current.starting_offset != expected_target.starting_offset
            || current.partition_length != expected_size
        {
            anyhow::bail!(
                "扩展操作返回成功，但操作后核验失败：期望 {} 字节，实际 {} 字节",
                expected_size,
                current.partition_length
            );
        }
        log::info!(
            "[CLEANUP] 分区 {} 扩展成功：{} MB -> {} MB",
            target_letter,
            size_before / 1024 / 1024,
            current.partition_length / 1024 / 1024
        );
        Ok(())
    }

    /// 无损扩大分区到指定大小（仅并入紧邻其后的未分配空间；不移动其它分区）。
    ///
    /// - `letter`：目标分区盘符（如 'C'）。在 PE 下应由扩容标记定位后传入。
    /// - `target_size_mb`：期望最终总大小（MB）；0 = 尽可能扩到最大（吃光相邻未分配空间）。
    ///
    /// 实现：VDS 只并入紧跟该卷的连续未分配空间；若其后是别的分区则失败关闭。
    pub fn expand_partition_lossless(letter: char, target_size_mb: u64) -> Result<String> {
        let current_mb = Self::get_partition_size_mb(letter)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法获取分区 {}: 的当前大小", letter)))?;
        log::info!(
            "[EXPAND] 目标分区 {}: 当前 {} MB，目标 {} MB",
            letter,
            current_mb,
            target_size_mb
        );

        let identity = lr_core::windows_storage::volume_identity(letter)?;
        let available = lr_core::windows_storage::contiguous_free_bytes_after(
            identity.disk_number,
            identity
                .offset_bytes
                .checked_add(identity.extent_length_bytes)
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
        lr_core::windows_storage::extend_volume(letter, identity.disk_number, bytes_to_add)?;
        let new_mb = Self::get_partition_size_mb(letter).unwrap_or(current_mb);
        let expected_mb = (identity.extent_length_bytes + bytes_to_add) / 1024 / 1024;
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
        partitions_are_physically_adjacent, DiskManager, PartitionGeometry, PartitionStyle,
    };

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
