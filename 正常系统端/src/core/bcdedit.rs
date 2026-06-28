use anyhow::Result;
use std::path::Path;

use crate::tr;
use crate::utils::cmd::create_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

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
        log::info!("[BOOT] 查找 {} 所在磁盘的 ESP 分区...", windows_partition);
        
        // 提取盘符（去掉冒号）
        let drive_letter = windows_partition.trim_end_matches(':').trim_end_matches('\\');
        
        // Step 1: 使用 diskpart 获取该分区所在的磁盘号
        let script1 = format!(r#"select volume {}
detail volume
"#, drive_letter);
        
        let script1_path = std::env::temp_dir().join("find_disk.txt");
        std::fs::write(&script1_path, &script1)?;
        
        let output = create_command("diskpart")
            .args(["/s", &script1_path.to_string_lossy()])
            .output()?;
        
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
        
        let script2_path = std::env::temp_dir().join("list_part.txt");
        std::fs::write(&script2_path, &script2)?;
        
        let output = create_command("diskpart")
            .args(["/s", &script2_path.to_string_lossy()])
            .output()?;
        
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
        
        // Step 3: 为 ESP 分配盘符
        // 先尝试移除可能存在的旧盘符
        let _ = create_command("mountvol").args(["S:", "/d"]).output();
        std::thread::sleep(std::time::Duration::from_millis(200));
        
        let script3 = format!(r#"select disk {}
select partition {}
assign letter=S
"#, disk_num, esp_partition);
        
        let script3_path = std::env::temp_dir().join("assign_esp.txt");
        std::fs::write(&script3_path, &script3)?;
        
        let output = create_command("diskpart")
            .args(["/s", &script3_path.to_string_lossy()])
            .output()?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        log::info!("[BOOT] 分配 ESP 盘符:\n{}", stdout);
        
        // 等待盘符生效
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        // 验证
        if Path::new("S:\\").exists() {
            log::info!("[BOOT] ESP 已挂载到 S:");
            Ok("S:".to_string())
        } else {
            anyhow::bail!("{}", tr!("ESP 盘符分配失败"))
        }
    }

    /// 查找并挂载 EFI 系统分区（旧方法，作为备选）
    pub fn find_and_mount_esp(&self) -> Result<String> {
        log::info!("[BOOT] 查找 EFI 系统分区...");
        
        // 方法1: 检查 S: 是否已经是 ESP
        if Path::new("S:\\EFI").exists() {
            log::info!("[BOOT] S: 已经是 ESP");
            return Ok("S:".to_string());
        }
        
        // 方法2: 使用 mountvol /s 挂载 ESP 到 S:
        log::info!("[BOOT] 尝试使用 mountvol /s 挂载 ESP");
        let output = create_command("mountvol").args(["S:", "/s"]).output();
        if output.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if Path::new("S:\\").exists() {
                log::info!("[BOOT] ESP 已通过 mountvol 挂载到 S:");
                return Ok("S:".to_string());
            }
        }
        
        // 方法3: 使用 diskpart 查找所有磁盘的 ESP
        self.find_esp_with_diskpart()
    }

    /// 使用 diskpart 查找任意磁盘上的 ESP
    fn find_esp_with_diskpart(&self) -> Result<String> {
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
assign letter=S
"#, disk, part_num);
                                    
                                    let assign_path = std::env::temp_dir().join("assign_esp2.txt");
                                    std::fs::write(&assign_path, &assign_script)?;
                                    
                                    let _ = create_command("diskpart")
                                        .args(["/s", &assign_path.to_string_lossy()])
                                        .output();
                                    
                                    std::thread::sleep(std::time::Duration::from_millis(500));
                                    
                                    if Path::new("S:\\").exists() {
                                        log::info!("[BOOT] 找到 ESP: 磁盘 {} 分区 {}", disk, part_num);
                                        return Ok("S:".to_string());
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
        self.repair_boot_advanced(windows_partition, true)
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
    pub fn repair_boot_advanced(&self, windows_partition: &str, use_uefi: bool) -> Result<()> {
        let windows_path = format!("{}\\Windows", windows_partition);
        
        log::info!("[BOOT] ========== 修复引导 ==========");
        log::info!("[BOOT] Windows 路径: {}", windows_path);
        log::info!("[BOOT] 引导模式: {}", if use_uefi { "UEFI" } else { "Legacy/BIOS" });

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("{}", tr!("Windows 目录不存在: {}", windows_path));
        }

        // 用户可编辑的修复引导脚本（bin\repair_boot.txt）——仅在「高级选项」开启时启用，优先于默认逻辑；
        // 失败则回退默认逻辑。小白默认关闭，避免一份误放的 repair_boot.txt 把引导改坏。
        let allow_custom_repair =
            crate::core::app_config::AppConfig::load().enable_advanced_options;
        let repair_script = get_bin_dir().join("repair_boot.txt");
        if allow_custom_repair && repair_script.exists() {
            log::info!("[BOOT] 检测到自定义修复引导脚本: {}", repair_script.display());
            let esp = if use_uefi {
                self.find_esp_on_same_disk(windows_partition)
                    .or_else(|_| self.find_and_mount_esp())
                    .ok()
            } else {
                None
            };
            match lr_core::boot::run_repair_script(
                &repair_script,
                &get_bin_dir(),
                windows_partition,
                use_uefi,
                esp.as_deref(),
            ) {
                Ok(out) => {
                    log::info!("[BOOT] 自定义修复引导脚本执行完成:\n{}", out);
                    return Ok(());
                }
                Err(e) => log::warn!("[BOOT] 自定义修复引导脚本失败，回退默认逻辑: {}", e),
            }
        }

        if use_uefi {
            // UEFI 模式：需要找到并挂载 ESP 分区
            log::info!("[BOOT] UEFI 模式：查找 ESP 分区");
            
            // 首先尝试在同一磁盘上查找 ESP
            let esp_result = self.find_esp_on_same_disk(windows_partition)
                .or_else(|_| self.find_and_mount_esp());
            
            match esp_result {
                Ok(esp_letter) => {
                    log::info!("[BOOT] ESP 分区: {}", esp_letter);
                    
                    // 确保 EFI 目录存在
                    let efi_ms_dir = format!("{}\\EFI\\Microsoft", esp_letter);
                    let efi_boot_dir = format!("{}\\EFI\\Boot", esp_letter);
                    
                    // 创建必要的目录
                    let _ = std::fs::create_dir_all(&efi_ms_dir);
                    let _ = std::fs::create_dir_all(&efi_boot_dir);
                    
                    // 使用 bcdboot 写入 UEFI 引导文件
                    // bcdboot C:\Windows /s S: /f UEFI /l zh-cn
                    log::info!("[BOOT] 执行: bcdboot {} /s {} /f UEFI /l zh-cn", windows_path, esp_letter);
                    let output = create_command(&self.bcdboot_path)
                        .args([
                            &windows_path,
                            "/s", &esp_letter,
                            "/f", "UEFI",
                            "/l", "zh-cn"
                        ])
                        .output()?;
                    
                    let stdout = gbk_to_utf8(&output.stdout);
                    let stderr = gbk_to_utf8(&output.stderr);
                    
                    log::info!("[BOOT] bcdboot stdout: {}", stdout);
                    log::info!("[BOOT] bcdboot stderr: {}", stderr);

                    if !output.status.success() {
                        // 尝试使用 ALL 参数（同时创建 UEFI 和 BIOS 引导）
                        log::info!("[BOOT] 重试：使用 ALL 模式");
                        let output = create_command(&self.bcdboot_path)
                            .args([
                                &windows_path,
                                "/s", &esp_letter,
                                "/f", "ALL",
                                "/l", "zh-cn"
                            ])
                            .output()?;
                        
                        let stdout = gbk_to_utf8(&output.stdout);
                        let stderr = gbk_to_utf8(&output.stderr);
                        log::info!("[BOOT] bcdboot (ALL) stdout: {}", stdout);
                        log::info!("[BOOT] bcdboot (ALL) stderr: {}", stderr);

                        if !output.status.success() {
                            // 最后尝试不指定 /f 参数
                            log::info!("[BOOT] 重试：不指定引导类型");
                            let output = create_command(&self.bcdboot_path)
                                .args([
                                    &windows_path,
                                    "/s", &esp_letter,
                                    "/l", "zh-cn"
                                ])
                                .output()?;
                            
                            let stderr = gbk_to_utf8(&output.stderr);
                            if !output.status.success() {
                                anyhow::bail!("{}", tr!("UEFI 引导修复失败: {}", stderr));
                            }
                        }
                    }
                    
                    // 验证引导文件是否创建成功
                    let bootmgfw = format!("{}\\EFI\\Microsoft\\Boot\\bootmgfw.efi", esp_letter);
                    let bootx64 = format!("{}\\EFI\\Boot\\bootx64.efi", esp_letter);
                    
                    if Path::new(&bootmgfw).exists() {
                        log::info!("[BOOT] 引导文件已创建: {}", bootmgfw);
                    } else {
                        log::warn!("[BOOT] 警告: 未找到 bootmgfw.efi");
                    }
                    
                    if Path::new(&bootx64).exists() {
                        log::info!("[BOOT] 引导文件已创建: {}", bootx64);
                    } else {
                        // 复制 bootmgfw.efi 到 bootx64.efi
                        if Path::new(&bootmgfw).exists() {
                            let _ = std::fs::copy(&bootmgfw, &bootx64);
                            log::info!("[BOOT] 已复制 bootmgfw.efi -> bootx64.efi");
                        }
                    }
                    
                    log::info!("[BOOT] UEFI 引导修复成功");
                }
                Err(e) => {
                    log::warn!("[BOOT] 查找 ESP 失败: {}，尝试默认方式", e);
                    
                    // 尝试默认方式（让 bcdboot 自动处理）
                    let output = create_command(&self.bcdboot_path)
                        .args([&windows_path, "/f", "UEFI", "/l", "zh-cn"])
                        .output()?;
                    
                    let stdout = gbk_to_utf8(&output.stdout);
                    let stderr = gbk_to_utf8(&output.stderr);
                    log::info!("[BOOT] bcdboot (auto) stdout: {}", stdout);
                    log::info!("[BOOT] bcdboot (auto) stderr: {}", stderr);
                    
                    if !output.status.success() {
                        anyhow::bail!("{}", tr!("引导修复失败: {}", stderr));
                    }
                }
            }
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
            .or_else(|_| self.find_and_mount_esp())
            .map_err(|e| anyhow::anyhow!("{}", tr!("未找到 ESP，无法写 UEFI 引导: {}", e)))?;
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
