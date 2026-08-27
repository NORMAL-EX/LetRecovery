use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::tr;
use crate::utils::cmd::create_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;
use lr_core::boot_pca::BootPcaMode;

static ESP_MOUNT_LOCK: Mutex<()> = Mutex::new(());

fn ensure_legacy_bootsect_success(
    success: bool,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    if success {
        return Ok(());
    }
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    anyhow::bail!(
        "{}",
        tr!(
            "bootsect 写入 Legacy 引导失败（退出码 {}）：{}",
            format!("{:?}", exit_code),
            detail
        )
    )
}

fn require_legacy_active_partition(result: Result<()>) -> Result<()> {
    result.map_err(|error| anyhow::anyhow!("{}", tr!("设置 Legacy 活动分区失败：{}", error)))
}

fn require_pca_esp(esp: Option<(u32, u32, u64)>) -> Result<(u32, u32, u64)> {
    esp.ok_or_else(|| anyhow::anyhow!("{}", tr!("目标磁盘上没有 EFI 系统分区。")))
}

fn highest_free_mount_letter(assigned_mask: u32) -> Option<char> {
    (3_u8..=25)
        .rev()
        .find(|index| assigned_mask & (1_u32 << index) == 0)
        .map(|index| char::from(b'A' + index))
}

fn available_esp_mount_letter() -> Result<char> {
    // GetLogicalDrives returning zero is an API failure, not evidence that every letter is used.
    // Preserve that distinction so WinPE service/device failures are never misreported as normal
    // drive-letter exhaustion. D-Z only mirrors the existing shared selection policy.
    let assigned = lr_core::windows_storage::assigned_drive_letter_mask()
        .map_err(|error| anyhow::anyhow!("{}", tr!("无法查询当前盘符分配状态: {}", error)))?;
    highest_free_mount_letter(assigned)
        .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有空闲盘符可挂载 ESP")))
}

pub(crate) fn finish_with_esp_cleanup<T>(
    operation: Result<T>,
    mount: Option<lr_core::boot_pca::TemporaryEspMountGuard>,
) -> Result<T> {
    let cleanup = mount
        .map(lr_core::boot_pca::TemporaryEspMountGuard::close)
        .unwrap_or(Ok(()));
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => anyhow::bail!("temporary ESP cleanup failed: {cleanup}"),
        (Err(operation), Ok(())) => Err(operation),
        (Err(operation), Err(cleanup)) => Err(anyhow::anyhow!(
            "{operation:#}; additionally temporary ESP cleanup failed: {cleanup}"
        )),
    }
}

pub struct BootManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl BootManager {
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        let bcdedit_path = lr_core::windows_compat::system_directory()
            .map(|directory| directory.join("bcdedit.exe"))
            .unwrap_or_else(|error| {
                log::error!("[BOOT] 无法解析宿主 System32，bcdedit 将失败关闭: {error}");
                PathBuf::from("__LetRecovery_missing_System32__").join("bcdedit.exe")
            });
        Self {
            bcdedit_path: bcdedit_path.to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    fn optional_esp_on_same_disk(
        &self,
        windows_partition: &str,
    ) -> Result<Option<(u32, u32, u64)>> {
        log::info!("[BOOT] 查找 {} 所在磁盘的 ESP 分区...", windows_partition);

        let drive_letter = windows_partition
            .trim_end_matches(':')
            .trim_end_matches('\\')
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法确定分区所在磁盘")))?
            .to_ascii_uppercase();
        let disks = super::quick_partition::get_physical_disks();
        let disk = disks
            .iter()
            .find(|disk| {
                disk.partitions.iter().any(|partition| {
                    partition
                        .drive_letter
                        .is_some_and(|letter| letter.eq_ignore_ascii_case(&drive_letter))
                })
            })
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法确定分区所在磁盘")))?;
        let disk_num = disk.disk_number;
        log::info!("[BOOT] 目标分区在磁盘 {}", disk_num);

        let Some(esp) = disk.partitions.iter().find(|partition| partition.is_esp) else {
            return Ok(None);
        };
        log::info!(
            "[BOOT] 找到 ESP: 分区 {}，偏移 {}",
            esp.partition_number,
            esp.offset_bytes
        );

        Ok(Some((disk_num, esp.partition_number, esp.offset_bytes)))
    }

    fn esp_on_same_disk(&self, windows_partition: &str) -> Result<(u32, u32, u64)> {
        self.optional_esp_on_same_disk(windows_partition)?
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("未找到 ESP 分区")))
    }

    fn mount_known_esp(
        &self,
        disk_num: u32,
        esp_offset: u64,
    ) -> Result<lr_core::boot_pca::TemporaryEspMountGuard> {
        let expected = lr_core::windows_storage::VolumeIdentity {
            disk_number: disk_num,
            offset_bytes: esp_offset,
            extent_length_bytes: 0,
        };
        let existing_letters =
            lr_core::windows_storage::assigned_drive_letters_for_partition(disk_num, esp_offset)?;
        if let Some(letter) = existing_letters.first().copied() {
            log::info!(
                "[BOOT] ESP 已有盘符 {}:，只读复用且不会在探测后移除",
                letter
            );
            return lr_core::boot_pca::TemporaryEspMountGuard::existing(&letter.to_string())
                .map_err(anyhow::Error::msg);
        }

        let mount_letter = available_esp_mount_letter()?;
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(disk_num)?;
        lr_core::windows_storage::assign_partition_drive_letter_checked(
            disk_num,
            esp_offset,
            mount_letter,
            &expected_layout,
        )?;
        let mount_guard = lr_core::boot_pca::TemporaryEspMountGuard::new(
            &mount_letter.to_string(),
            expected,
            expected_layout,
        )
        .map_err(anyhow::Error::msg)?;

        let mount_root = format!("{}:\\", mount_letter);
        for _ in 0..20 {
            if Path::new(&mount_root).is_dir() {
                log::info!("[BOOT] ESP 已挂载到 {}:", mount_letter);
                return Ok(mount_guard);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let cleanup = mount_guard.close();
        match cleanup {
            Ok(()) => anyhow::bail!("{}", tr!("ESP 盘符分配失败")),
            Err(error) => {
                anyhow::bail!("{}", tr!("ESP 盘符分配失败，且临时盘符卸载失败: {}", error))
            }
        }
    }

    /// 查找目标 Windows 分区所在磁盘的 ESP 分区
    pub fn find_esp_on_same_disk(
        &self,
        windows_partition: &str,
    ) -> Result<lr_core::boot_pca::TemporaryEspMountGuard> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (disk_num, _partition_number, esp_offset) = self.esp_on_same_disk(windows_partition)?;
        self.mount_known_esp(disk_num, esp_offset)
    }

    /// Inspect the existing Windows boot manager on the ESP that belongs to
    /// `windows_partition`. The Windows 10/11 fast path uses the volume GUID root without changing
    /// access paths. Windows 7 can omit a hidden ESP from that namespace, so the documented VDS
    /// partition interface supplies a scoped, identity-checked drive letter that is removed before
    /// this method returns. The installer performs a fresh source and firmware check before writing.
    pub fn inspect_existing_esp_pca(
        &self,
        windows_partition: &str,
    ) -> Result<lr_core::boot_pca::EfiSignatureInfo> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (disk_number, _partition_number, offset_bytes) =
            require_pca_esp(self.optional_esp_on_same_disk(windows_partition)?)?;

        let result = lr_core::boot_pca::inspect_existing_esp_with_fallback(
            || {
                lr_core::windows_storage::try_volume_guid_path_for_partition(
                    disk_number,
                    offset_bytes,
                )
                .map(|path| path.map(Into::into))
                .map_err(|error| error.to_string())
            },
            || {
                log::warn!(
                    "[BOOT PCA] 目标 ESP 未通过卷 GUID 路径开放，使用受物理身份约束的临时盘符只读检测"
                );
                let guard = self
                    .mount_known_esp(disk_number, offset_bytes)
                    .map_err(|error| error.to_string())?;
                let root = std::path::PathBuf::from(format!("{}\\", guard.letter()));
                Ok((root, guard))
            },
            |root| {
                if !root.is_dir() {
                    return Err(format!("ESP root is not accessible: {}", root.display()));
                }
                Ok(lr_core::boot_pca::inspect_esp_generation(root))
            },
            lr_core::boot_pca::TemporaryEspMountGuard::close,
        );

        match result {
            Ok(info) => Ok(info),
            Err(error @ lr_core::boot_pca::EspProbeAccessError::Cleanup { .. }) => {
                log::error!("[BOOT PCA] 临时 ESP 卸载失败: {}", error);
                anyhow::bail!(
                    "{}",
                    tr!("目标磁盘上的临时 ESP 盘符卸载失败。为安全起见，已阻止继续安装；请查看日志。")
                )
            }
            Err(error) => {
                log::error!("[BOOT PCA] ESP 已找到但无法解析或挂载: {}", error);
                anyhow::bail!(
                    "{}",
                    tr!("目标磁盘上的 ESP 已存在，但无法解析或临时挂载；请查看日志。")
                )
            }
        }
    }

    fn find_esp_with_windows_api(&self) -> Result<lr_core::boot_pca::TemporaryEspMountGuard> {
        log::info!("[BOOT] 使用 WinAPI 查找 ESP");
        for disk in crate::core::quick_partition::get_physical_disks() {
            for partition in disk.partitions.iter().filter(|partition| partition.is_esp) {
                match self.mount_known_esp(disk.disk_number, partition.offset_bytes) {
                    Ok(mount_guard) => {
                        log::info!(
                            "[BOOT] 找到 ESP: 磁盘 {} 分区 {}",
                            disk.disk_number,
                            partition.partition_number
                        );
                        return Ok(mount_guard);
                    }
                    Err(error) => {
                        log::warn!(
                            "[BOOT] 无法挂载磁盘 {} 分区 {}: {}",
                            disk.disk_number,
                            partition.partition_number,
                            error
                        );
                    }
                }
            }
        }

        anyhow::bail!("{}", tr!("未找到 EFI 系统分区"))
    }

    /// 修复指定分区的引导（简单版本）
    pub fn repair_boot(&self, windows_partition: &str) -> Result<()> {
        self.repair_boot_advanced(windows_partition, true, BootPcaMode::Auto)
    }

    /// Legacy/MBR：在 windows_partition 所在磁盘上确定【引导分区】并挂好盘符（照搬 DSI）。
    ///
    /// System+Windows 拆分布局时，bootmgr/BCD 应写到【活动的 System 分区】而不是 Windows 分区；
    /// 单分区/无独立 System 分区时则用 Windows 分区自身作引导分区，稍后把它设为活动——逻辑一致。
    ///
    /// 活动分区判定走 IOCTL（直接读 MBR BootIndicator 引导字节），不再解析 diskpart 文本：
    /// 新版 Windows 的 `detail partition` 可能不显示"活动"字段，`list partition` 的 `*` 又只表示焦点，
    /// 两种文本解析都不可靠。给独立 System 分区挂一个盘符以便 bcdboot /s 指过去。
    /// 返回 (引导分区盘符如 "S:", 磁盘号, 分区号)。
    fn prepare_legacy_boot_partition(
        &self,
        windows_partition: &str,
    ) -> Result<(
        String,
        usize,
        usize,
        Option<lr_core::boot_pca::TemporaryEspMountGuard>,
    )> {
        let wl_char = windows_partition
            .trim_end_matches('\\')
            .trim_end_matches(':')
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase());

        // 用 IOCTL 扫描所有物理盘，定位 Windows 分区所在磁盘号 + 分区号（权威，不依赖盘符枚举）。
        let disks = crate::core::quick_partition::get_physical_disks();
        let mut disk_num: Option<u32> = None;
        let mut win_part: Option<u32> = None;
        'outer: for d in &disks {
            for p in &d.partitions {
                if let (Some(dl), Some(wc)) = (p.drive_letter, wl_char) {
                    if dl.to_ascii_uppercase() == wc {
                        disk_num = Some(d.disk_number);
                        win_part = Some(p.partition_number);
                        break 'outer;
                    }
                }
            }
        }
        let disk_num = disk_num.ok_or_else(|| {
            anyhow::anyhow!(
                "无法确定 {} 所在磁盘（IOCTL 未匹配到盘符）",
                windows_partition
            )
        })?;
        let win_part = win_part.unwrap_or(0);

        // 该磁盘的活动（引导）分区——权威来源：MBR BootIndicator=0x80（复用上面同一次 IOCTL 扫描）。
        let active = disks
            .iter()
            .find(|d| d.disk_number == disk_num)
            .and_then(|d| d.partitions.iter().find(|p| p.is_active))
            .map(|p| p.partition_number);

        match active {
            // 独立的活动 System 分区（≠Windows 分区）：引导写到它，给它挂个盘符供 bcdboot /s。
            Some(ap) if ap != 0 && ap != win_part => {
                let (letter, mount) = self.letter_for_partition(&disks, disk_num, ap)?;
                log::info!(
                    "[BOOT] Legacy 引导分区 = 活动 System 分区 磁盘{}:分区{} -> {}",
                    disk_num,
                    ap,
                    letter
                );
                Ok((letter, disk_num as usize, ap as usize, mount))
            }
            // 活动分区就是 Windows 分区，或本盘没有活动分区：用 Windows 分区自身作引导分区，
            // 稍后由调用方将其设为活动。Windows 分区已挂好盘符，直接用。
            _ => {
                log::info!(
                    "[BOOT] Legacy 引导分区 = Windows 分区自身 磁盘{}:分区{} -> {}",
                    disk_num,
                    win_part,
                    windows_partition
                );
                Ok((
                    windows_partition.to_string(),
                    disk_num as usize,
                    win_part as usize,
                    None,
                ))
            }
        }
    }

    /// 取 磁盘:分区 的盘符——【有就用、没有才分配空闲盘符】，绝不 remove 已有盘符。
    fn letter_for_partition(
        &self,
        disks: &[crate::core::quick_partition::PhysicalDisk],
        disk_num: u32,
        part: u32,
    ) -> Result<(String, Option<lr_core::boot_pca::TemporaryEspMountGuard>)> {
        // 先看 IOCTL 扫描结果里这个分区有没有现成盘符。
        let existing = disks
            .iter()
            .find(|d| d.disk_number == disk_num)
            .and_then(|d| d.partitions.iter().find(|p| p.partition_number == part))
            .and_then(|p| p.drive_letter);
        if let Some(c) = existing {
            let letter = format!("{}:", c.to_ascii_uppercase());
            if Path::new(&format!("{}\\", letter)).exists() {
                return Ok((letter, None));
            }
        }
        // 没有则通过 VDS 给它分配一个空闲盘符。
        let free = crate::core::disk::DiskManager::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("没有空闲盘符可分配给引导分区"))?;
        let offset = disks
            .iter()
            .find(|disk| disk.disk_number == disk_num)
            .and_then(|disk| {
                disk.partitions
                    .iter()
                    .find(|partition| partition.partition_number == part)
            })
            .map(|partition| (partition.offset_bytes, partition.size_bytes))
            .ok_or_else(|| anyhow::anyhow!("无法重新定位引导分区"))?;
        let (offset, size) = offset;
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(disk_num)?;
        lr_core::windows_storage::assign_partition_drive_letter_checked(
            disk_num,
            offset,
            free,
            &expected_layout,
        )?;
        let letter = format!("{}:", free);
        let mount = lr_core::boot_pca::TemporaryEspMountGuard::new(
            &letter,
            lr_core::windows_storage::VolumeIdentity {
                disk_number: disk_num,
                offset_bytes: offset,
                extent_length_bytes: size,
            },
            expected_layout,
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..20 {
            if Path::new(&format!("{}\\", letter)).exists() {
                return Ok((letter, Some(mount)));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !Path::new(&format!("{}\\", letter)).exists() {
            anyhow::bail!(
                "引导分区 磁盘{}:分区{} 盘符 {} 不可用",
                disk_num,
                part,
                letter
            );
        }
        Ok((letter, Some(mount)))
    }

    /// 把指定 磁盘:分区 设为活动分区（Legacy/MBR 引导必需，照搬 DSI 的 PART *a）。
    fn set_partition_active(&self, disk_num: usize, part_num: usize) -> Result<()> {
        let partition = crate::core::quick_partition::get_physical_disks()
            .into_iter()
            .find(|disk| disk.disk_number == disk_num as u32)
            .and_then(|disk| {
                disk.partitions
                    .into_iter()
                    .find(|partition| partition.partition_number == part_num as u32)
            })
            .ok_or_else(|| anyhow::anyhow!("无法重新定位要设为活动的分区"))?;
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(disk_num as u32)?;
        lr_core::windows_storage::set_mbr_active_checked(
            disk_num as u32,
            partition.offset_bytes,
            true,
            &expected_layout,
        )?;
        log::info!(
            "[BOOT] 已通过 WinAPI 设活动分区 磁盘{}:分区{}",
            disk_num,
            part_num
        );
        Ok(())
    }

    /// 按盘符把卷所在分区设为活动（磁盘:分区号未知时的兜底）。
    fn set_partition_active_by_letter(&self, boot_letter: &str) -> Result<()> {
        let letter = boot_letter
            .trim_end_matches('\\')
            .trim_end_matches(':')
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("引导分区盘符为空"))?;
        let identity = lr_core::windows_storage::volume_identity(letter)?;
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(identity.disk_number)?;
        lr_core::windows_storage::set_mbr_active_checked(
            identity.disk_number,
            identity.offset_bytes,
            true,
            &expected_layout,
        )?;
        log::info!("[BOOT] 已通过 WinAPI 设活动分区 卷{}:", letter);
        Ok(())
    }

    /// 修复指定分区的引导（高级版本，支持指定引导模式）
    pub fn repair_boot_advanced(
        &self,
        windows_partition: &str,
        use_uefi: bool,
        pca_mode: BootPcaMode,
    ) -> Result<()> {
        let windows_path = format!("{}\\Windows", windows_partition);

        log::info!("[BOOT] ========== 修复引导 ==========");
        log::info!("[BOOT] Windows 路径: {}", windows_path);
        log::info!(
            "[BOOT] 引导模式: {}",
            if use_uefi { "UEFI" } else { "Legacy/BIOS" }
        );

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("{}", tr!("Windows 目录不存在: {}", windows_path));
        }

        let mounted_esp = if use_uefi {
            log::info!("[BOOT] UEFI 模式：查找目标磁盘 ESP 分区");
            Some(
                self.find_esp_on_same_disk(windows_partition)
                    .map_err(|error| {
                        anyhow::anyhow!("{}", tr!("目标系统所在磁盘没有可用的 ESP: {}", error))
                    })?,
            )
        } else {
            None
        };
        let mut legacy_mount = None;
        let operation = (|| -> Result<()> {
            let existing_esp_hint = mounted_esp.as_ref().map(|mount| {
                let esp_letter = mount.letter();
                let esp_root = format!("{}\\", esp_letter.trim_end_matches('\\'));
                let info = lr_core::boot_pca::inspect_esp_generation(Path::new(&esp_root));
                if info.signature_valid {
                    info.generation
                } else {
                    lr_core::boot_pca::PcaGeneration::Unknown
                }
            });

            if use_uefi {
                let esp_letter = mounted_esp
                    .as_ref()
                    .map(lr_core::boot_pca::TemporaryEspMountGuard::letter)
                    .expect("UEFI repair always mounts the target-disk ESP first");
                let (version, family) =
                    lr_core::boot_pca::inspect_installed_windows_boot_family(windows_partition)
                        .map_err(|error| {
                            anyhow::anyhow!("{}", tr!("无法确认目标系统引导版本: {}", error))
                        })?;
                log::info!(
                    "[BOOT] 目标 Windows 版本: {}，引导族: {:?}",
                    version,
                    family
                );
                match family {
                    lr_core::boot_pca::InstalledWindowsBootFamily::LegacyUefi => {
                        lr_core::boot_pca::repair_legacy_windows_uefi_boot(
                            Path::new(&self.bcdboot_path),
                            windows_partition,
                            esp_letter,
                        )
                        .map_err(|error| {
                            anyhow::anyhow!("{}", tr!("旧版 Windows UEFI 引导修复失败: {}", error))
                        })?;
                        log::info!("[BOOT] 旧版 Windows 标准 UEFI 引导修复成功");
                    }
                    lr_core::boot_pca::InstalledWindowsBootFamily::ModernPca => {
                        let firmware = lr_core::boot_pca::inspect_firmware_pca();
                        log::info!("[BOOT PCA] 固件检测: {:?}", firmware);
                        let decision = lr_core::boot_pca::repair_uefi_boot(
                            Path::new(&self.bcdboot_path),
                            windows_partition,
                            esp_letter,
                            pca_mode,
                            firmware,
                            existing_esp_hint,
                        )
                        .map_err(|error| {
                            anyhow::anyhow!("{}", tr!("UEFI 引导修复失败: {}", error))
                        })?;
                        log::info!(
                            "[BOOT] UEFI 引导修复成功: {} ({})",
                            decision.generation,
                            decision.reason
                        );
                    }
                    lr_core::boot_pca::InstalledWindowsBootFamily::Nt5 => {
                        anyhow::bail!("{}", tr!("NT5 系统必须使用 XP/2003 专用引导写入路径"));
                    }
                }
            } else {
                // Legacy/BIOS 模式——照搬 DSI：bootmgr/BCD 写到【活动的 System 分区】，而不是 Windows 分区。
                // System+Windows 拆分布局时引导分区≠Windows 分区（之前直接拿 Windows 分区写引导，导致开机 0x7B）；
                // 单分区布局时活动分区就是 Windows 分区，逻辑一致。
                log::info!("[BOOT] Legacy 模式：写入 MBR 引导");

                let (boot_letter, boot_disk, boot_part, mount) =
                    self.prepare_legacy_boot_partition(windows_partition)?;
                legacy_mount = mount;
                log::info!(
                    "[BOOT] Legacy 引导分区: {} (磁盘{}:分区{})",
                    boot_letter,
                    boot_disk,
                    boot_part
                );

                // Bootsect 是后续承重步骤，先确认依赖存在，避免 BCDBoot 已改盘后才发现无法完成。
                let bootsect_path = get_bin_dir().join("bootsect.exe");
                if !bootsect_path.is_file() {
                    anyhow::bail!(
                        "{}",
                        tr!(
                            "Legacy 引导修复缺少 bootsect.exe：{}",
                            bootsect_path.display()
                        )
                    );
                }

                // 1) bcdboot W:\Windows /s <引导分区> /f BIOS /l zh-cn（/s 指定系统分区——关键差异）
                let out = create_command(&self.bcdboot_path)
                    .args([
                        windows_path.as_str(),
                        "/s",
                        boot_letter.as_str(),
                        "/f",
                        "BIOS",
                        "/l",
                        "zh-cn",
                    ])
                    .output()?;
                log::info!(
                    "[BOOT] bcdboot /s {}: stdout={} stderr={}",
                    boot_letter,
                    gbk_to_utf8(&out.stdout),
                    gbk_to_utf8(&out.stderr)
                );
                if !out.status.success() {
                    // Windows 7 compatibility: retry without /f, but never drop /s. Removing /s
                    // would allow BCDBoot to choose a system partition on a different disk.
                    let retry = create_command(&self.bcdboot_path)
                        .args([
                            windows_path.as_str(),
                            "/s",
                            boot_letter.as_str(),
                            "/l",
                            "zh-cn",
                        ])
                        .output()?;
                    if !retry.status.success() {
                        anyhow::bail!(
                            "{}",
                            tr!("Legacy 引导修复失败: {}", gbk_to_utf8(&retry.stderr))
                        );
                    }
                }

                // 2) bootsect /nt60 <引导分区> /force /mbr（写【引导分区】的引导扇区 + MBR 引导码）
                let out = create_command(&bootsect_path)
                    .args(["/nt60", boot_letter.as_str(), "/force", "/mbr"])
                    .output()?;
                let bootsect_stdout = gbk_to_utf8(&out.stdout);
                let bootsect_stderr = gbk_to_utf8(&out.stderr);
                log::info!(
                    "[BOOT] bootsect /nt60 {} /force /mbr: stdout={} stderr={}",
                    boot_letter,
                    bootsect_stdout,
                    bootsect_stderr
                );
                ensure_legacy_bootsect_success(
                    out.status.success(),
                    out.status.code(),
                    &bootsect_stdout,
                    &bootsect_stderr,
                )?;

                // 3) 把引导分区设为活动（DSI 的 PART *a）——Legacy/MBR 开机的承重步骤，两条路径都要做。
                //    有磁盘:分区号就按号设；走了回退(boot_part==0、磁盘/分区号未知)则按引导盘符兜底设活动，
                //    避免"clean 后新建分区从未设活动 → 写完引导文件磁盘仍无活动分区 → BIOS 找不到引导设备 0x7B"。
                require_legacy_active_partition(if boot_part > 0 {
                    self.set_partition_active(boot_disk, boot_part)
                } else {
                    self.set_partition_active_by_letter(&boot_letter)
                })?;

                log::info!("[BOOT] Legacy 引导修复成功");
            }

            log::info!("[BOOT] ========== 引导修复完成 ==========");
            Ok(())
        })();
        let operation = finish_with_esp_cleanup(operation, mounted_esp);
        finish_with_esp_cleanup(operation, legacy_mount)
    }

    /// 为已释放的 XP/2003 系统写入引导（ntldr/boot.ini + MBR，仅 Legacy）。
    pub fn write_xp_boot(&self, windows_partition: &str) -> Result<()> {
        log::info!("[BOOT] ========== 写入 XP 引导 ==========");
        match lr_core::boot::write_xp_boot(&get_bin_dir(), windows_partition) {
            Ok(out) => {
                log::info!("[BOOT] XP 引导写入完成:\n{}", out);
                Ok(())
            }
            Err(e) => anyhow::bail!("{}", tr!("XP 引导写入失败: {}", e)),
        }
    }

    /// 为已释放的「UEFI 化」XP/2003 系统写入 UEFI/GPT 引导（用映像自带 bootxp64.efi + BCC）。
    ///
    /// 查找同盘 ESP 并挂载，再复刻社区方案写 UEFI 引导。映像若不含 UEFI 引导文件，返回 Err
    /// 让调用方回退 Legacy。
    pub fn write_xp_uefi_gpt_boot(&self, windows_partition: &str) -> Result<()> {
        log::info!("[BOOT] ========== 写入 XP UEFI/GPT 引导 ==========");
        let esp = self
            .find_esp_on_same_disk(windows_partition)
            .map_err(|e| anyhow::anyhow!("{}", tr!("未找到 ESP，无法写 UEFI 引导: {}", e)))?;
        log::info!("[BOOT] 使用 ESP: {}", esp.letter());
        let operation = (|| -> Result<()> {
            match lr_core::xp::write_xp_uefi_gpt_boot(
                windows_partition,
                esp.letter(),
                Path::new(&self.bcdedit_path),
            ) {
                Ok(out) => {
                    log::info!("[BOOT] XP UEFI 引导写入完成:\n{}", out);
                    Ok(())
                }
                Err(e) => anyhow::bail!("{}", tr!("XP UEFI 引导写入失败: {}", e)),
            }
        })();
        finish_with_esp_cleanup(operation, Some(esp))
    }
}

impl Default for BootManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_bootsect_nonzero_exit_fails_closed_with_stderr() {
        let error = ensure_legacy_bootsect_success(false, Some(5), "partial", "access denied")
            .unwrap_err()
            .to_string();

        assert!(error.contains("Some(5)"));
        assert!(error.contains("access denied"));
        assert!(!error.contains("partial"));
    }

    #[test]
    fn legacy_bootsect_success_is_accepted() {
        ensure_legacy_bootsect_success(true, Some(0), "ok", "").unwrap();
    }

    #[test]
    fn legacy_active_partition_failure_is_propagated() {
        let error = require_legacy_active_partition(Err(anyhow::anyhow!("modeled failure")))
            .unwrap_err()
            .to_string();

        assert!(error.contains("modeled failure"));
    }

    #[test]
    fn missing_esp_is_distinct_from_an_access_failure() {
        let error = require_pca_esp(None).unwrap_err().to_string();
        assert!(error.contains("没有 EFI 系统分区"));
        assert!(!error.contains("无法解析"));
        assert!(!error.contains("无法挂载"));
    }

    #[test]
    fn esp_mount_letter_prefers_highest_free_and_never_uses_abc() {
        assert_eq!(highest_free_mount_letter(0), Some('Z'));
        let z_and_y_used = (1_u32 << (b'Z' - b'A')) | (1_u32 << (b'Y' - b'A'));
        assert_eq!(highest_free_mount_letter(z_and_y_used), Some('X'));
        let all_d_through_z = (3_u8..=25).fold(0_u32, |mask, index| mask | (1_u32 << index));
        assert_eq!(highest_free_mount_letter(all_d_through_z), None);
        assert_eq!(highest_free_mount_letter(all_d_through_z | 0b111), None);
    }
}
