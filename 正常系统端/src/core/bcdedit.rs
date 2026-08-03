use anyhow::Result;
use std::path::Path;
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

pub struct BootManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl BootManager {
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        Self {
            bcdedit_path: bin_dir.join("bcdedit.exe").to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    /// 获取当前系统引导 GUID
    pub fn get_current_boot_guid(&self) -> Result<String> {
        let output = create_command(&self.bcdedit_path)
            .args(["/enum"])
            .output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        let system_drive = format!(
            "{}:",
            lr_core::windows_storage::current_windows_drive_letter()?
        );

        let mut current_guid = String::new();
        for line in stdout.lines() {
            if line.starts_with("identifier") || line.contains("标识符") {
                if let Some(guid) = line.split_whitespace().last() {
                    current_guid = guid.to_string();
                }
            }
            if line.contains("device") && line.contains(&system_drive) {
                return Ok(current_guid);
            }
        }

        anyhow::bail!("Could not find current boot GUID")
    }

    /// 查找目标 Windows 分区所在磁盘的 ESP 分区
    pub fn find_esp_on_same_disk(&self, windows_partition: &str) -> Result<String> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

        let esp = disk
            .partitions
            .iter()
            .find(|partition| partition.is_esp)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("未找到 ESP 分区")))?;
        log::info!(
            "[BOOT] 找到 ESP: 分区 {}，偏移 {}",
            esp.partition_number,
            esp.offset_bytes
        );

        // Step 3: 使用真正空闲的盘符挂载 ESP，不能覆盖用户已有的 S: 等盘符。
        let mount_letter = lr_core::boot_pca::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有空闲盘符可挂载 ESP")))?;

        lr_core::windows_storage::assign_partition_drive_letter(
            disk_num,
            esp.offset_bytes,
            mount_letter,
        )?;

        let mount_root = format!("{}:\\", mount_letter);
        for _ in 0..20 {
            if Path::new(&mount_root).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if Path::new(&mount_root).exists() {
            let mounted = format!("{}:", mount_letter);
            log::info!("[BOOT] ESP 已挂载到 {}", mounted);
            Ok(mounted)
        } else {
            let _ = lr_core::boot_pca::unmount_esp(&mount_letter.to_string());
            anyhow::bail!("{}", tr!("ESP 盘符分配失败"))
        }
    }

    /// Inspect the existing Windows boot manager on the ESP that belongs to
    /// `windows_partition`. This is used only as an automatic-selection signal;
    /// the installer performs a fresh source and firmware check before writing.
    pub fn inspect_existing_esp_pca(
        &self,
        windows_partition: &str,
    ) -> Result<lr_core::boot_pca::EfiSignatureInfo> {
        let esp_letter = self.find_esp_on_same_disk(windows_partition)?;
        let esp_mount = lr_core::boot_pca::TemporaryEspMountGuard::new(&esp_letter)
            .map_err(anyhow::Error::msg)?;
        let esp_root = format!("{}\\", esp_mount.letter().trim_end_matches('\\'));
        let result = lr_core::boot_pca::inspect_esp_generation(Path::new(&esp_root));
        Ok(result)
    }

    /// 查找并挂载 EFI 系统分区（旧方法，作为备选）
    pub fn find_and_mount_esp(&self) -> Result<String> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        log::info!("[BOOT] 查找 EFI 系统分区...");

        let mount_letter = lr_core::boot_pca::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有空闲盘符可挂载 ESP")))?;

        // 通过 IOCTL/VDS 枚举全部物理磁盘上的 ESP 并分配访问路径。
        self.find_esp_with_windows_api(mount_letter)
    }

    fn find_esp_with_windows_api(&self, mount_letter: char) -> Result<String> {
        log::info!("[BOOT] 使用 WinAPI 查找 ESP");
        for disk in crate::core::quick_partition::get_physical_disks() {
            for partition in disk.partitions.iter().filter(|partition| partition.is_esp) {
                if let Err(error) = lr_core::windows_storage::assign_partition_drive_letter(
                    disk.disk_number,
                    partition.offset_bytes,
                    mount_letter,
                ) {
                    log::warn!(
                        "[BOOT] 无法挂载磁盘 {} 分区 {}: {}",
                        disk.disk_number,
                        partition.partition_number,
                        error
                    );
                    continue;
                }
                let mount_root = format!("{}:\\", mount_letter);
                for _ in 0..20 {
                    if Path::new(&mount_root).exists() {
                        log::info!(
                            "[BOOT] 找到 ESP: 磁盘 {} 分区 {}",
                            disk.disk_number,
                            partition.partition_number
                        );
                        return Ok(format!("{}:", mount_letter));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        anyhow::bail!("{}", tr!("未找到 EFI 系统分区"))
    }

    /// 设置默认引导项
    pub fn set_default_boot(&self, guid: &str) -> Result<()> {
        let output = create_command(&self.bcdedit_path)
            .args(["/default", guid])
            .output()?;

        if !output.status.success() {
            anyhow::bail!("Failed to set default boot entry");
        }
        Ok(())
    }

    /// 设置引导超时
    pub fn set_timeout(&self, seconds: u32) -> Result<()> {
        let output = create_command(&self.bcdedit_path)
            .args(["/timeout", &seconds.to_string()])
            .output()?;

        if !output.status.success() {
            anyhow::bail!("Failed to set boot timeout");
        }
        Ok(())
    }

    /// 删除引导项
    pub fn delete_boot_entry(&self, guid: &str) -> Result<()> {
        let output = create_command(&self.bcdedit_path)
            .args(["/delete", guid, "/f"])
            .output()?;

        if !output.status.success() {
            anyhow::bail!("Failed to delete boot entry");
        }
        Ok(())
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
    ) -> Result<(String, usize, usize)> {
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
                let letter = self.letter_for_partition(&disks, disk_num, ap)?;
                log::info!(
                    "[BOOT] Legacy 引导分区 = 活动 System 分区 磁盘{}:分区{} -> {}",
                    disk_num,
                    ap,
                    letter
                );
                Ok((letter, disk_num as usize, ap as usize))
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
    ) -> Result<String> {
        // 先看 IOCTL 扫描结果里这个分区有没有现成盘符。
        let existing = disks
            .iter()
            .find(|d| d.disk_number == disk_num)
            .and_then(|d| d.partitions.iter().find(|p| p.partition_number == part))
            .and_then(|p| p.drive_letter);
        if let Some(c) = existing {
            let letter = format!("{}:", c.to_ascii_uppercase());
            if Path::new(&format!("{}\\", letter)).exists() {
                return Ok(letter);
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
            .map(|partition| partition.offset_bytes)
            .ok_or_else(|| anyhow::anyhow!("无法重新定位引导分区"))?;
        lr_core::windows_storage::assign_partition_drive_letter(disk_num, offset, free)?;
        let letter = format!("{}:", free);
        for _ in 0..20 {
            if Path::new(&format!("{}\\", letter)).exists() {
                return Ok(letter);
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
        Ok(letter)
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
        lr_core::windows_storage::set_mbr_active(disk_num as u32, partition.offset_bytes, true)?;
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
        lr_core::windows_storage::set_mbr_active(
            identity.disk_number,
            identity.offset_bytes,
            true,
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
        let _esp_mount_guard = mounted_esp
            .as_deref()
            .map(lr_core::boot_pca::TemporaryEspMountGuard::new)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        let existing_esp_hint = mounted_esp.as_deref().map(|esp_letter| {
            let esp_root = format!("{}\\", esp_letter.trim_end_matches('\\'));
            let info = lr_core::boot_pca::inspect_esp_generation(Path::new(&esp_root));
            if info.signature_valid {
                info.generation
            } else {
                lr_core::boot_pca::PcaGeneration::Unknown
            }
        });

        // 用户可编辑的修复引导脚本（bin\repair_boot.txt）仅在「高级选项」开启时启用。
        // Legacy 成功后可直接完成；UEFI 命令作为前置步骤，随后仍由内置逻辑按所选 PCA
        // 重新写入并校验，避免自定义命令绕过 Secure Boot 兼容性检查。
        let allow_custom_repair =
            crate::core::app_config::AppConfig::load().enable_advanced_options;
        let repair_script = get_bin_dir().join("repair_boot.txt");
        if allow_custom_repair && repair_script.exists() {
            log::info!(
                "[BOOT] 检测到自定义修复引导脚本: {}",
                repair_script.display()
            );
            match lr_core::boot::run_repair_script(
                &repair_script,
                &get_bin_dir(),
                windows_partition,
                use_uefi,
                mounted_esp.as_deref(),
            ) {
                Ok(out) => {
                    log::info!("[BOOT] 自定义修复引导脚本执行完成:\n{}", out);
                    if !use_uefi {
                        return Ok(());
                    }
                    log::info!("[BOOT PCA] 将继续执行内置 UEFI 写入与签名验证");
                }
                Err(e) => log::warn!("[BOOT] 自定义修复引导脚本失败，回退默认逻辑: {}", e),
            }
        }

        if use_uefi {
            let esp_letter = mounted_esp
                .as_deref()
                .expect("UEFI repair always mounts the target-disk ESP first");

            let firmware = lr_core::boot_pca::inspect_firmware_pca();
            log::info!("[BOOT PCA] 固件检测: {:?}", firmware);

            let repair_result = lr_core::boot_pca::repair_uefi_boot(
                Path::new(&self.bcdboot_path),
                windows_partition,
                esp_letter,
                pca_mode,
                firmware,
                existing_esp_hint,
            );
            let decision = repair_result
                .map_err(|error| anyhow::anyhow!("{}", tr!("UEFI 引导修复失败: {}", error)))?;
            log::info!(
                "[BOOT] UEFI 引导修复成功: {} ({})",
                decision.generation,
                decision.reason
            );
        } else {
            // Legacy/BIOS 模式——照搬 DSI：bootmgr/BCD 写到【活动的 System 分区】，而不是 Windows 分区。
            // System+Windows 拆分布局时引导分区≠Windows 分区（之前直接拿 Windows 分区写引导，导致开机 0x7B）；
            // 单分区布局时活动分区就是 Windows 分区，逻辑一致。
            log::info!("[BOOT] Legacy 模式：写入 MBR 引导");

            // 找引导（活动）分区并挂好盘符；找不到则回退用 Windows 分区自身（老行为，至少不更差）。
            let (boot_letter, boot_disk, boot_part) =
                match self.prepare_legacy_boot_partition(windows_partition) {
                    Ok(t) => t,
                    Err(e) => {
                        log::warn!(
                            "[BOOT] 未找到引导/活动分区({})，回退用系统分区自身写引导",
                            e
                        );
                        (windows_partition.to_string(), 0usize, 0usize)
                    }
                };
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
                // 回退1：不带 /s（让 bcdboot 自己挑活动分区）
                let out2 = create_command(&self.bcdboot_path)
                    .args([windows_path.as_str(), "/f", "BIOS", "/l", "zh-cn"])
                    .output()?;
                if !out2.status.success() {
                    // 回退2：不带 /f
                    let out3 = create_command(&self.bcdboot_path)
                        .args([windows_path.as_str(), "/l", "zh-cn"])
                        .output()?;
                    if !out3.status.success() {
                        anyhow::bail!(
                            "{}",
                            tr!("Legacy 引导修复失败: {}", gbk_to_utf8(&out3.stderr))
                        );
                    }
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
    }

    /// 查找 EFI 分区
    pub fn find_efi_partition(&self) -> Result<String> {
        self.find_and_mount_esp()
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
        let _esp_mount_guard =
            lr_core::boot_pca::TemporaryEspMountGuard::new(&esp).map_err(anyhow::Error::msg)?;
        log::info!("[BOOT] 使用 ESP: {}", esp);
        match lr_core::xp::write_xp_uefi_gpt_boot(
            windows_partition,
            &esp,
            Path::new(&self.bcdedit_path),
        ) {
            Ok(out) => {
                log::info!("[BOOT] XP UEFI 引导写入完成:\n{}", out);
                Ok(())
            }
            Err(e) => anyhow::bail!("{}", tr!("XP UEFI 引导写入失败: {}", e)),
        }
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
}
