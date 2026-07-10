use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::tr;
use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;
use lr_core::boot_pca::BootPcaMode;

static DISKPART_SCRIPT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static ESP_MOUNT_LOCK: Mutex<()> = Mutex::new(());

pub struct BootManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl BootManager {
    /// 选择一个可靠的临时目录并确保它存在（避免 WinPE 下 os error 3）。
    fn reliable_temp_dir() -> PathBuf {
        // 统一走 system_utils::get_temp_directory（按 SystemRoot/PE系统盘动态解析，不写死 X:）
        crate::core::system_utils::get_temp_directory()
    }

    fn run_diskpart_script(script: &str, purpose: &str) -> Result<std::process::Output> {
        let sequence = DISKPART_SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let script_path = Self::reliable_temp_dir().join(format!(
            "lr_{purpose}_{}_{}.txt",
            std::process::id(),
            sequence
        ));
        std::fs::write(&script_path, script)?;
        let script_arg = script_path.to_string_lossy().into_owned();
        let output = new_command("diskpart").args(["/s", &script_arg]).output();
        let _ = std::fs::remove_file(&script_path);
        Ok(output?)
    }

    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        Self {
            bcdedit_path: bin_dir
                .join("bcdedit.exe")
                .to_string_lossy()
                .to_string(),
            bcdboot_path: bin_dir
                .join("bcdboot.exe")
                .to_string_lossy()
                .to_string(),
        }
    }

    /// 查找目标 Windows 分区所在磁盘的 ESP 分区
    pub fn find_esp_on_same_disk(&self, windows_partition: &str) -> Result<String> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        log::info!("查找 {} 所在磁盘的 ESP 分区...", windows_partition);

        let drive_letter = windows_partition
            .trim_end_matches(':')
            .trim_end_matches('\\');

        // Step 1: 使用 diskpart 获取该分区所在的磁盘号
        let script1 = format!(
            r#"select volume {}
detail volume
"#,
            drive_letter
        );

        let output = Self::run_diskpart_script(&script1, "find_disk")?;

        let stdout = gbk_to_utf8(&output.stdout);
        log::debug!("查找磁盘号:\n{}", stdout);

        let mut disk_num: Option<usize> = None;
        for line in stdout.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("disk") || line_lower.contains("磁盘") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if part.to_lowercase().contains("disk") || *part == "磁盘" {
                        if let Some(num_str) = parts.get(i + 1) {
                            if let Ok(num) = num_str.parse::<usize>() {
                                disk_num = Some(num);
                                break;
                            }
                        }
                    }
                }
            }
        }

        let disk_num = disk_num.ok_or_else(|| anyhow::anyhow!("{}", tr!("无法确定分区所在磁盘")))?;
        log::info!("目标分区在磁盘 {}", disk_num);

        // Step 2: 查找该磁盘上的 ESP 分区
        let script2 = format!(
            r#"select disk {}
list partition
"#,
            disk_num
        );

        let output = Self::run_diskpart_script(&script2, "list_partitions")?;

        let stdout = gbk_to_utf8(&output.stdout);
        log::debug!("分区列表:\n{}", stdout);

        let mut esp_partition: Option<usize> = None;
        for line in stdout.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("system") || line_lower.contains("系统") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if part.to_lowercase().contains("partition") || *part == "分区" {
                        if let Some(num_str) = parts.get(i + 1) {
                            if let Ok(num) = num_str.parse::<usize>() {
                                esp_partition = Some(num);
                                log::info!("找到 ESP: 分区 {}", num);
                                break;
                            }
                        }
                    }
                }
                if esp_partition.is_some() {
                    break;
                }
            }
        }

        let esp_partition = esp_partition.ok_or_else(|| anyhow::anyhow!("{}", tr!("未找到 ESP 分区")))?;

        // Step 3: 使用真正空闲的盘符挂载 ESP，不能覆盖用户已有的 S: 等盘符。
        let mount_letter = lr_core::boot_pca::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有空闲盘符可挂载 ESP")))?;

        let script3 = format!(
            r#"select disk {}
select partition {}
assign letter={}
"#,
            disk_num, esp_partition, mount_letter
        );

        let output = Self::run_diskpart_script(&script3, "assign_esp")?;

        let stdout = gbk_to_utf8(&output.stdout);
        log::debug!("分配 ESP 盘符:\n{}", stdout);

        std::thread::sleep(std::time::Duration::from_millis(500));

        let mount_root = format!("{}:\\", mount_letter);
        if Path::new(&mount_root).exists() {
            let mounted = format!("{}:", mount_letter);
            log::info!("ESP 已挂载到 {}", mounted);
            Ok(mounted)
        } else {
            let _ = lr_core::boot_pca::unmount_esp(&mount_letter.to_string());
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

        // 忽略失败，因为可能本来就没有这个引导项
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
        let _ = self.delete_current_boot_entry();

        let mounted_esp = if use_uefi {
            log::info!("UEFI 模式：查找目标磁盘 ESP 分区");
            Some(self.find_esp_on_same_disk(windows_partition).map_err(|error| {
                anyhow::anyhow!(
                    "{}",
                    tr!("目标系统所在磁盘没有可用的 ESP: {}", error)
                )
            })?)
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
                mounted_esp.as_deref(),
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
                "UEFI 引导修复成功: {} ({})",
                decision.generation,
                decision.reason
            );
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
        let _ = self.delete_current_boot_entry();
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
        let _ = self.delete_current_boot_entry();

        let esp = self
            .find_esp_on_same_disk(windows_partition)
            .map_err(|e| anyhow::anyhow!("{}", tr!("未找到 ESP，无法写 UEFI 引导: {}", e)))?;
        let _esp_mount_guard = lr_core::boot_pca::TemporaryEspMountGuard::new(&esp)
            .map_err(anyhow::Error::msg)?;
        log::info!("使用 ESP: {}", esp);

        match lr_core::xp::write_xp_uefi_gpt_boot(
            windows_partition,
            &esp,
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
