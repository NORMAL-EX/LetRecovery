use anyhow::Result;
use std::path::Path;
use std::sync::Mutex;

use crate::tr;
use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;
use lr_core::boot_pca::BootPcaMode;

static ESP_MOUNT_LOCK: Mutex<()> = Mutex::new(());

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

    /// 查找目标 Windows 分区所在磁盘的 ESP 分区
    pub fn find_esp_on_same_disk(
        &self,
        windows_partition: &str,
    ) -> Result<lr_core::boot_pca::TemporaryEspMountGuard> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        log::info!("查找 {} 所在磁盘的 ESP 分区...", windows_partition);

        let drive_letter = windows_partition
            .trim_end_matches(':')
            .trim_end_matches('\\')
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法确定分区所在磁盘")))?;
        let target = lr_core::windows_storage::volume_identity(drive_letter)?;
        let disk_num = target.disk_number;
        log::info!("目标分区在磁盘 {}", disk_num);

        let esp = lr_core::windows_storage::partitions(disk_num)?
            .into_iter()
            .find(|partition| partition.kind == lr_core::windows_storage::PartitionKind::EfiSystem)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("未找到 ESP 分区")))?;
        log::info!("找到 ESP: 分区 {}", esp.partition_number);

        let existing_letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
            disk_num,
            esp.offset_bytes,
        )?;
        if let Some(letter) = existing_letters.first().copied() {
            log::info!("ESP 已有盘符 {}:，复用且不会在操作后移除", letter);
            return lr_core::boot_pca::TemporaryEspMountGuard::existing(&letter.to_string())
                .map_err(anyhow::Error::msg);
        }

        // Step 3: 使用真正空闲的盘符挂载 ESP，不能覆盖用户已有的 S: 等盘符。
        let mount_letter = lr_core::boot_pca::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有空闲盘符可挂载 ESP")))?;

        lr_core::windows_storage::assign_partition_drive_letter(
            disk_num,
            esp.offset_bytes,
            mount_letter,
        )?;
        let mount_guard = lr_core::boot_pca::TemporaryEspMountGuard::new(&mount_letter.to_string())
            .map_err(anyhow::Error::msg)?;

        let mount_root = format!("{}:\\", mount_letter);
        for _ in 0..20 {
            if Path::new(&mount_root).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if Path::new(&mount_root).exists() {
            let mounted = format!("{}:", mount_letter);
            log::info!("ESP 已挂载到 {}", mounted);
            Ok(mount_guard)
        } else {
            anyhow::bail!("{}", tr!("ESP 盘符分配失败"))
        }
    }

    /// 删除当前PE引导项
    pub fn delete_current_boot_entry(&self) -> Result<()> {
        log::info!("删除当前PE引导项...");

        let output = new_command(&self.bcdedit_path)
            .args(["/delete", "{current}", "/f"])
            .output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);

        log::debug!("bcdedit delete stdout: {}", stdout);
        log::debug!("bcdedit delete stderr: {}", stderr);

        if !output.status.success() {
            anyhow::bail!(
                "{}",
                tr!(
                    "删除当前 PE 引导项失败（退出码 {}）：{}",
                    format!("{:?}", output.status.code()),
                    if stderr.trim().is_empty() {
                        stdout.trim()
                    } else {
                        stderr.trim()
                    }
                )
            );
        }
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

        log::info!("========== 修复引导 ==========");
        log::info!("Windows 路径: {}", windows_path);
        log::info!(
            "引导模式: {}",
            if use_uefi { "UEFI" } else { "Legacy/BIOS" }
        );

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("{}", tr!("Windows 目录不存在: {}", windows_path));
        }

        // 先删除当前PE引导项
        self.delete_current_boot_entry()?;

        let mounted_esp = if use_uefi {
            log::info!("UEFI 模式：查找目标磁盘 ESP 分区");
            Some(
                self.find_esp_on_same_disk(windows_partition)
                    .map_err(|error| {
                        anyhow::anyhow!("{}", tr!("目标系统所在磁盘没有可用的 ESP: {}", error))
                    })?,
            )
        } else {
            None
        };
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

        // Legacy 自定义脚本成功后可直接完成；UEFI 命令作为前置步骤，随后仍由
        // 内置逻辑按所选 PCA 重新写入并校验，不能绕过 Secure Boot 兼容性检查。
        let repair_script = get_bin_dir().join("repair_boot.txt");
        if repair_script.exists() {
            log::info!("检测到自定义修复引导脚本: {}", repair_script.display());
            match lr_core::boot::run_repair_script(
                &repair_script,
                &get_bin_dir(),
                windows_partition,
                use_uefi,
                mounted_esp.as_ref().map(|mount| mount.letter()),
            ) {
                Ok(out) => {
                    log::info!("自定义修复引导脚本执行完成:\n{}", out);
                    if !use_uefi {
                        log::info!("========== 引导修复完成（自定义脚本）==========");
                        return Ok(());
                    }
                    log::info!("[BOOT PCA] 将继续执行内置 UEFI 写入与签名验证");
                }
                Err(e) => log::warn!("自定义修复引导脚本失败，回退默认逻辑: {}", e),
            }
        }

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
                    log::info!("旧版 Windows 标准 UEFI 引导修复成功");
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
                    .map_err(|error| anyhow::anyhow!("{}", tr!("UEFI 引导修复失败: {}", error)))?;
                    log::info!(
                        "UEFI 引导修复成功: {} ({})",
                        decision.generation,
                        decision.reason
                    );
                }
                lr_core::boot_pca::InstalledWindowsBootFamily::Nt5 => {
                    anyhow::bail!("{}", tr!("NT5 系统必须使用 XP/2003 专用引导写入路径"));
                }
            }
        } else {
            // Legacy/BIOS 模式
            log::info!("Legacy 模式：写入 MBR 引导");

            let bootsect_path = get_bin_dir().join("bootsect.exe");
            if bootsect_path.exists() {
                log::info!("使用 bootsect 写入引导扇区");
                let output = new_command(&bootsect_path)
                    .args(["/nt60", windows_partition, "/mbr"])
                    .output()?;

                let stdout = gbk_to_utf8(&output.stdout);
                let stderr = gbk_to_utf8(&output.stderr);
                log::debug!("bootsect stdout: {}", stdout);
                log::debug!("bootsect stderr: {}", stderr);
                if !output.status.success() {
                    anyhow::bail!(
                        "{}",
                        tr!(
                            "bootsect 写入引导扇区失败（退出码 {}）：{}",
                            format!("{:?}", output.status.code()),
                            if stderr.trim().is_empty() {
                                stdout.trim()
                            } else {
                                stderr.trim()
                            }
                        )
                    );
                }
            }

            let output = new_command(&self.bcdboot_path)
                .args([&windows_path, "/f", "BIOS", "/l", "zh-cn"])
                .output()?;

            let stdout = gbk_to_utf8(&output.stdout);
            let stderr = gbk_to_utf8(&output.stderr);

            log::debug!("bcdboot stdout: {}", stdout);
            log::debug!("bcdboot stderr: {}", stderr);

            if !output.status.success() {
                let output = new_command(&self.bcdboot_path)
                    .args([&windows_path, "/l", "zh-cn"])
                    .output()?;

                let stderr = gbk_to_utf8(&output.stderr);
                if !output.status.success() {
                    anyhow::bail!("{}", tr!("Legacy 引导修复失败: {}", stderr));
                }
            }

            log::info!("Legacy 引导修复成功");
        }

        log::info!("========== 引导修复完成 ==========");
        Ok(())
    }

    /// 为已释放的 XP/2003 系统写入引导（ntldr/boot.ini + MBR，仅 Legacy）。
    pub fn write_xp_boot(&self, windows_partition: &str) -> Result<()> {
        log::info!("========== 写入 XP 引导 ==========");
        self.delete_current_boot_entry()?;
        match lr_core::boot::write_xp_boot(&get_bin_dir(), windows_partition) {
            Ok(out) => {
                log::info!("XP 引导写入完成:\n{}", out);
                log::info!("========== XP 引导完成 ==========");
                Ok(())
            }
            Err(e) => anyhow::bail!("{}", tr!("XP 引导写入失败: {}", e)),
        }
    }

    /// 为已释放的「UEFI 化」XP/2003 系统写入 UEFI/GPT 引导。
    ///
    /// 查找同盘 ESP 并挂载，再用映像自带的 `WINDOWS\Boot\EFI`（bootxp64.efi + BCC）复刻
    /// 社区方案的 UEFI 引导写入。映像若不含这些文件，返回 Err，调用方应回退 Legacy。
    pub fn write_xp_uefi_gpt_boot(&self, windows_partition: &str) -> Result<()> {
        log::info!("========== 写入 XP UEFI/GPT 引导 ==========");
        self.delete_current_boot_entry()?;

        let esp = self
            .find_esp_on_same_disk(windows_partition)
            .map_err(|e| anyhow::anyhow!("{}", tr!("未找到 ESP，无法写 UEFI 引导: {}", e)))?;
        log::info!("使用 ESP: {}", esp.letter());

        match lr_core::xp::write_xp_uefi_gpt_boot(
            windows_partition,
            esp.letter(),
            Path::new(&self.bcdedit_path),
        ) {
            Ok(out) => {
                log::info!("XP UEFI 引导写入完成:\n{}", out);
                log::info!("========== XP UEFI 引导完成 ==========");
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
