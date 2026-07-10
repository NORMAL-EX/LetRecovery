use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::tr;
use crate::utils::cmd::create_command;
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
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        Self {
            bcdedit_path: bin_dir.join("bcdedit.exe").to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    fn run_diskpart_script(script: &str, purpose: &str) -> Result<std::process::Output> {
        let sequence = DISKPART_SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let script_path = std::env::temp_dir().join(format!(
            "lr_{purpose}_{}_{}.txt",
            std::process::id(),
            sequence
        ));
        std::fs::write(&script_path, script)?;
        let script_arg = script_path.to_string_lossy().into_owned();
        let output = create_command("diskpart").args(["/s", &script_arg]).output();
        let _ = std::fs::remove_file(&script_path);
        Ok(output?)
    }

    /// 获取当前系统引导 GUID
    pub fn get_current_boot_guid(&self) -> Result<String> {
        let output = create_command(&self.bcdedit_path).args(["/enum"]).output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());

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
        
        // 提取盘符（去掉冒号）
        let drive_letter = windows_partition.trim_end_matches(':').trim_end_matches('\\');
        
        // Step 1: 使用 diskpart 获取该分区所在的磁盘号
        let script1 = format!(r#"select volume {}
detail volume
"#, drive_letter);
        
        let output = Self::run_diskpart_script(&script1, "find_disk")?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        log::info!("[BOOT] 查找磁盘号:\n{}", stdout);
        
        // 解析磁盘号
        let mut disk_num: Option<usize> = None;
        for line in stdout.lines() {
            let line_lower = line.to_lowercase();
            // 查找 "Disk 0" 或 "磁盘 0"
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
        log::info!("[BOOT] 目标分区在磁盘 {}", disk_num);
        
        // Step 2: 查找该磁盘上的 ESP 分区（使用 GPT 类型）
        let script2 = format!(r#"select disk {}
list partition
"#, disk_num);
        
        let output = Self::run_diskpart_script(&script2, "list_partitions")?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        log::info!("[BOOT] 分区列表:\n{}", stdout);
        
        // 查找 System/系统 类型的分区（ESP）
        let mut esp_partition: Option<usize> = None;
        for line in stdout.lines() {
            let line_lower = line.to_lowercase();
            // 查找 "System" 或 "系统" 类型的分区
            if line_lower.contains("system") || line_lower.contains("系统") {
                // 提取分区号
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if part.to_lowercase().contains("partition") || *part == "分区" {
                        if let Some(num_str) = parts.get(i + 1) {
                            if let Ok(num) = num_str.parse::<usize>() {
                                esp_partition = Some(num);
                                log::info!("[BOOT] 找到 ESP: 分区 {}", num);
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
        
        let script3 = format!(r#"select disk {}
select partition {}
assign letter={}
"#, disk_num, esp_partition, mount_letter);
        
        let output = Self::run_diskpart_script(&script3, "assign_esp")?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        log::info!("[BOOT] 分配 ESP 盘符:\n{}", stdout);
        
        // 等待盘符生效
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        // 验证
        let mount_root = format!("{}:\\", mount_letter);
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
        let mounted = format!("{}:", mount_letter);
        let mount_root = format!("{}\\", mounted);

        // 方法1: 使用 mountvol /s 挂载当前系统 ESP。
        log::info!("[BOOT] 尝试使用 mountvol /s 挂载 ESP 到 {}", mounted);
        let output = create_command("mountvol").args([mounted.as_str(), "/s"]).output();
        if output.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if Path::new(&mount_root).exists() {
                log::info!("[BOOT] ESP 已通过 mountvol 挂载到 {}", mounted);
                return Ok(mounted);
            }
        }

        // 方法2: 使用 diskpart 查找所有磁盘的 ESP。
        self.find_esp_with_diskpart(mount_letter)
    }

    /// 使用 diskpart 查找任意磁盘上的 ESP
    fn find_esp_with_diskpart(&self, mount_letter: char) -> Result<String> {
        log::info!("[BOOT] 使用 diskpart 查找 ESP");
        
        // 遍历磁盘0-3
        for disk in 0..4 {
            let script = format!(r#"select disk {}
list partition
"#, disk);
            
            let script_path = std::env::temp_dir().join("check_disk.txt");
            std::fs::write(&script_path, &script)?;
            
            let output = create_command("diskpart")
                .args(["/s", &script_path.to_string_lossy()])
                .output()?;
            
            let stdout = gbk_to_utf8(&output.stdout);
            
            // 查找 System 类型分区
            for line in stdout.lines() {
                let line_lower = line.to_lowercase();
                if line_lower.contains("system") || line_lower.contains("系统") {
                    // 提取分区号
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    for (i, part) in parts.iter().enumerate() {
                        if part.to_lowercase().contains("partition") || *part == "分区" {
                            if let Some(num_str) = parts.get(i + 1) {
                                if let Ok(part_num) = num_str.parse::<usize>() {
                                    // 找到了，分配盘符
                                    let assign_script = format!(r#"select disk {}
select partition {}
assign letter={}
"#, disk, part_num, mount_letter);
                                    
                                    let assign_path = std::env::temp_dir().join("assign_esp2.txt");
                                    std::fs::write(&assign_path, &assign_script)?;
                                    
                                    let _ = create_command("diskpart")
                                        .args(["/s", &assign_path.to_string_lossy()])
                                        .output();
                                    
                                    std::thread::sleep(std::time::Duration::from_millis(500));
                                    
                                    let mount_root = format!("{}:\\", mount_letter);
                                    if Path::new(&mount_root).exists() {
                                        log::info!("[BOOT] 找到 ESP: 磁盘 {} 分区 {}", disk, part_num);
                                        return Ok(format!("{}:", mount_letter));
                                    }
                                }
                            }
                        }
                    }
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
    fn prepare_legacy_boot_partition(&self, windows_partition: &str) -> Result<(String, usize, usize)> {
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
        let disk_num = disk_num
            .ok_or_else(|| anyhow::anyhow!("无法确定 {} 所在磁盘（IOCTL 未匹配到盘符）", windows_partition))?;
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
                    disk_num, ap, letter
                );
                Ok((letter, disk_num as usize, ap as usize))
            }
            // 活动分区就是 Windows 分区，或本盘没有活动分区：用 Windows 分区自身作引导分区，
            // 稍后由调用方将其设为活动。Windows 分区已挂好盘符，直接用。
            _ => {
                log::info!(
                    "[BOOT] Legacy 引导分区 = Windows 分区自身 磁盘{}:分区{} -> {}",
                    disk_num, win_part, windows_partition
                );
                Ok((windows_partition.to_string(), disk_num as usize, win_part as usize))
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
        // 没有则用 diskpart 给它分配一个空闲盘符。
        let free = crate::core::disk::DiskManager::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("没有空闲盘符可分配给引导分区"))?;
        let script = format!(
            "select disk {}\r\nselect partition {}\r\nassign letter={}\r\n",
            disk_num, part, free
        );
        let p = std::env::temp_dir().join("lr_bp_asg.txt");
        std::fs::write(&p, script.as_bytes())?;
        let _ = create_command("diskpart").args(["/s", &p.to_string_lossy()]).output()?;
        let _ = std::fs::remove_file(&p);
        std::thread::sleep(std::time::Duration::from_millis(600));
        let letter = format!("{}:", free);
        if !Path::new(&format!("{}\\", letter)).exists() {
            anyhow::bail!("引导分区 磁盘{}:分区{} 盘符 {} 不可用", disk_num, part, letter);
        }
        Ok(letter)
    }

    /// 把指定 磁盘:分区 设为活动分区（Legacy/MBR 引导必需，照搬 DSI 的 PART *a）。
    fn set_partition_active(&self, disk_num: usize, part_num: usize) -> Result<()> {
        let script = format!(
            "select disk {}\r\nselect partition {}\r\nactive\r\n",
            disk_num, part_num
        );
        let p = std::env::temp_dir().join("lr_set_active.txt");
        std::fs::write(&p, script.as_bytes())?;
        let out = create_command("diskpart").args(["/s", &p.to_string_lossy()]).output()?;
        let _ = std::fs::remove_file(&p);
        log::info!(
            "[BOOT] 设活动分区 磁盘{}:分区{}: {}",
            disk_num,
            part_num,
            gbk_to_utf8(&out.stdout).trim()
        );
        Ok(())
    }

    /// 按盘符把卷所在分区设为活动（磁盘:分区号未知时的兜底）。
    /// diskpart `active` 作用于当前焦点分区，`select volume <letter>` 先把焦点落到该卷即可。
    fn set_partition_active_by_letter(&self, boot_letter: &str) -> Result<()> {
        let vol = boot_letter.trim_end_matches('\\').trim_end_matches(':');
        let script = format!("select volume {}\r\nactive\r\n", vol);
        let p = std::env::temp_dir().join("lr_set_active_vol.txt");
        std::fs::write(&p, script.as_bytes())?;
        let out = create_command("diskpart").args(["/s", &p.to_string_lossy()]).output()?;
        let _ = std::fs::remove_file(&p);
        log::info!("[BOOT] 设活动分区 卷{}: {}", vol, gbk_to_utf8(&out.stdout).trim());
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
        log::info!("[BOOT] 引导模式: {}", if use_uefi { "UEFI" } else { "Legacy/BIOS" });

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("{}", tr!("Windows 目录不存在: {}", windows_path));
        }

        let mounted_esp = if use_uefi {
            log::info!("[BOOT] UEFI 模式：查找目标磁盘 ESP 分区");
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

        // 用户可编辑的修复引导脚本（bin\repair_boot.txt）仅在「高级选项」开启时启用。
        // Legacy 成功后可直接完成；UEFI 命令作为前置步骤，随后仍由内置逻辑按所选 PCA
        // 重新写入并校验，避免自定义命令绕过 Secure Boot 兼容性检查。
        let allow_custom_repair =
            crate::core::app_config::AppConfig::load().enable_advanced_options;
        let repair_script = get_bin_dir().join("repair_boot.txt");
        if allow_custom_repair && repair_script.exists() {
            log::info!("[BOOT] 检测到自定义修复引导脚本: {}", repair_script.display());
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
                        log::warn!("[BOOT] 未找到引导/活动分区({})，回退用系统分区自身写引导", e);
                        (windows_partition.to_string(), 0usize, 0usize)
                    }
                };
            log::info!("[BOOT] Legacy 引导分区: {} (磁盘{}:分区{})", boot_letter, boot_disk, boot_part);

            // 1) bcdboot W:\Windows /s <引导分区> /f BIOS /l zh-cn（/s 指定系统分区——关键差异）
            let out = create_command(&self.bcdboot_path)
                .args([windows_path.as_str(), "/s", boot_letter.as_str(), "/f", "BIOS", "/l", "zh-cn"])
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
                        anyhow::bail!("{}", tr!("Legacy 引导修复失败: {}", gbk_to_utf8(&out3.stderr)));
                    }
                }
            }

            // 2) bootsect /nt60 <引导分区> /force /mbr（写【引导分区】的引导扇区 + MBR 引导码）
            let bootsect_path = get_bin_dir().join("bootsect.exe");
            if bootsect_path.exists() {
                let out = create_command(&bootsect_path)
                    .args(["/nt60", boot_letter.as_str(), "/force", "/mbr"])
                    .output()?;
                log::info!(
                    "[BOOT] bootsect /nt60 {} /force /mbr: {}",
                    boot_letter,
                    gbk_to_utf8(&out.stdout)
                );
            }

            // 3) 把引导分区设为活动（DSI 的 PART *a）——Legacy/MBR 开机的承重步骤，两条路径都要做。
            //    有磁盘:分区号就按号设；走了回退(boot_part==0、磁盘/分区号未知)则按引导盘符兜底设活动，
            //    避免"clean 后新建分区从未设活动 → 写完引导文件磁盘仍无活动分区 → BIOS 找不到引导设备 0x7B"。
            let active_res = if boot_part > 0 {
                self.set_partition_active(boot_disk, boot_part)
            } else {
                self.set_partition_active_by_letter(&boot_letter)
            };
            if let Err(e) = active_res {
                log::warn!("[BOOT] 设活动分区失败（忽略）: {}", e);
            }

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
        let _esp_mount_guard = lr_core::boot_pca::TemporaryEspMountGuard::new(&esp)
            .map_err(anyhow::Error::msg)?;
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
