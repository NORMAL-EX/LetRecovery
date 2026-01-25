use anyhow::Result;
use std::path::Path;

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
        println!("[BOOT] 查找 {} 所在磁盘的 ESP 分区...", windows_partition);
        
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
        println!("[BOOT] 查找磁盘号:\n{}", stdout);
        
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
        
        let disk_num = disk_num.ok_or_else(|| anyhow::anyhow!("无法确定分区所在磁盘"))?;
        println!("[BOOT] 目标分区在磁盘 {}", disk_num);
        
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
        println!("[BOOT] 分区列表:\n{}", stdout);
        
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
                                println!("[BOOT] 找到 ESP: 分区 {}", num);
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
        
        let esp_partition = esp_partition.ok_or_else(|| anyhow::anyhow!("未找到 ESP 分区"))?;
        
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
        println!("[BOOT] 分配 ESP 盘符:\n{}", stdout);
        
        // 等待盘符生效
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        // 验证
        if Path::new("S:\\").exists() {
            println!("[BOOT] ESP 已挂载到 S:");
            Ok("S:".to_string())
        } else {
            anyhow::bail!("ESP 盘符分配失败")
        }
    }

    /// 查找并挂载 EFI 系统分区（旧方法，作为备选）
    pub fn find_and_mount_esp(&self) -> Result<String> {
        println!("[BOOT] 查找 EFI 系统分区...");
        
        // 方法1: 检查 S: 是否已经是 ESP
        if Path::new("S:\\EFI").exists() {
            println!("[BOOT] S: 已经是 ESP");
            return Ok("S:".to_string());
        }
        
        // 方法2: 使用 mountvol /s 挂载 ESP 到 S:
        println!("[BOOT] 尝试使用 mountvol /s 挂载 ESP");
        let output = create_command("mountvol").args(["S:", "/s"]).output();
        if output.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if Path::new("S:\\").exists() {
                println!("[BOOT] ESP 已通过 mountvol 挂载到 S:");
                return Ok("S:".to_string());
            }
        }
        
        // 方法3: 使用 diskpart 查找所有磁盘的 ESP
        self.find_esp_with_diskpart()
    }

    /// 使用 diskpart 查找任意磁盘上的 ESP
    fn find_esp_with_diskpart(&self) -> Result<String> {
        println!("[BOOT] 使用 diskpart 查找 ESP");
        
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
                                        println!("[BOOT] 找到 ESP: 磁盘 {} 分区 {}", disk, part_num);
                                        return Ok("S:".to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        anyhow::bail!("未找到 EFI 系统分区")
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

    /// 修复指定分区的引导（高级版本，支持指定引导模式）
    pub fn repair_boot_advanced(&self, windows_partition: &str, use_uefi: bool) -> Result<()> {
        let windows_path = format!("{}\\Windows", windows_partition);
        
        println!("[BOOT] ========== 修复引导 ==========");
        println!("[BOOT] Windows 路径: {}", windows_path);
        println!("[BOOT] 引导模式: {}", if use_uefi { "UEFI" } else { "Legacy/BIOS" });

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("Windows 目录不存在: {}", windows_path);
        }

        if use_uefi {
            // UEFI 模式：需要找到并挂载 ESP 分区
            println!("[BOOT] UEFI 模式：查找 ESP 分区");
            
            // 首先尝试在同一磁盘上查找 ESP
            let esp_result = self.find_esp_on_same_disk(windows_partition)
                .or_else(|_| self.find_and_mount_esp());
            
            match esp_result {
                Ok(esp_letter) => {
                    println!("[BOOT] ESP 分区: {}", esp_letter);
                    
                    // 确保 EFI 目录存在
                    let efi_ms_dir = format!("{}\\EFI\\Microsoft", esp_letter);
                    let efi_boot_dir = format!("{}\\EFI\\Boot", esp_letter);
                    
                    // 创建必要的目录
                    let _ = std::fs::create_dir_all(&efi_ms_dir);
                    let _ = std::fs::create_dir_all(&efi_boot_dir);
                    
                    // 使用 bcdboot 写入 UEFI 引导文件
                    // bcdboot C:\Windows /s S: /f UEFI /l zh-cn
                    println!("[BOOT] 执行: bcdboot {} /s {} /f UEFI /l zh-cn", windows_path, esp_letter);
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
                    
                    println!("[BOOT] bcdboot stdout: {}", stdout);
                    println!("[BOOT] bcdboot stderr: {}", stderr);
                    
                    if !output.status.success() {
                        // 尝试使用 ALL 参数（同时创建 UEFI 和 BIOS 引导）
                        println!("[BOOT] 重试：使用 ALL 模式");
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
                        println!("[BOOT] bcdboot (ALL) stdout: {}", stdout);
                        println!("[BOOT] bcdboot (ALL) stderr: {}", stderr);
                        
                        if !output.status.success() {
                            // 最后尝试不指定 /f 参数
                            println!("[BOOT] 重试：不指定引导类型");
                            let output = create_command(&self.bcdboot_path)
                                .args([
                                    &windows_path,
                                    "/s", &esp_letter,
                                    "/l", "zh-cn"
                                ])
                                .output()?;
                            
                            let stderr = gbk_to_utf8(&output.stderr);
                            if !output.status.success() {
                                anyhow::bail!("UEFI 引导修复失败: {}", stderr);
                            }
                        }
                    }
                    
                    // 验证引导文件是否创建成功
                    let bootmgfw = format!("{}\\EFI\\Microsoft\\Boot\\bootmgfw.efi", esp_letter);
                    let bootx64 = format!("{}\\EFI\\Boot\\bootx64.efi", esp_letter);
                    
                    if Path::new(&bootmgfw).exists() {
                        println!("[BOOT] 引导文件已创建: {}", bootmgfw);
                    } else {
                        println!("[BOOT] 警告: 未找到 bootmgfw.efi");
                    }
                    
                    if Path::new(&bootx64).exists() {
                        println!("[BOOT] 引导文件已创建: {}", bootx64);
                    } else {
                        // 复制 bootmgfw.efi 到 bootx64.efi
                        if Path::new(&bootmgfw).exists() {
                            let _ = std::fs::copy(&bootmgfw, &bootx64);
                            println!("[BOOT] 已复制 bootmgfw.efi -> bootx64.efi");
                        }
                    }
                    
                    println!("[BOOT] UEFI 引导修复成功");
                }
                Err(e) => {
                    println!("[BOOT] 查找 ESP 失败: {}，尝试默认方式", e);
                    
                    // 尝试默认方式（让 bcdboot 自动处理）
                    let output = create_command(&self.bcdboot_path)
                        .args([&windows_path, "/f", "UEFI", "/l", "zh-cn"])
                        .output()?;
                    
                    let stdout = gbk_to_utf8(&output.stdout);
                    let stderr = gbk_to_utf8(&output.stderr);
                    println!("[BOOT] bcdboot (auto) stdout: {}", stdout);
                    println!("[BOOT] bcdboot (auto) stderr: {}", stderr);
                    
                    if !output.status.success() {
                        anyhow::bail!("引导修复失败: {}", stderr);
                    }
                }
            }
        } else {
            // Legacy/BIOS 模式
            println!("[BOOT] Legacy 模式：写入 MBR 引导");
            
            // 使用 bootsect 写入引导扇区
            let bootsect_path = get_bin_dir().join("bootsect.exe");
            if bootsect_path.exists() {
                println!("[BOOT] 使用 bootsect 写入引导扇区");
                let output = create_command(&bootsect_path)
                    .args(["/nt60", windows_partition, "/mbr"])
                    .output()?;
                
                let stdout = gbk_to_utf8(&output.stdout);
                let stderr = gbk_to_utf8(&output.stderr);
                println!("[BOOT] bootsect stdout: {}", stdout);
                println!("[BOOT] bootsect stderr: {}", stderr);
            }
            
            // bcdboot C:\Windows /f BIOS /l zh-cn
            let output = create_command(&self.bcdboot_path)
                .args([
                    &windows_path,
                    "/f", "BIOS",
                    "/l", "zh-cn"
                ])
                .output()?;
            
            let stdout = gbk_to_utf8(&output.stdout);
            let stderr = gbk_to_utf8(&output.stderr);
            
            println!("[BOOT] bcdboot stdout: {}", stdout);
            println!("[BOOT] bcdboot stderr: {}", stderr);
            
            if !output.status.success() {
                // 尝试不指定 /f 参数
                let output = create_command(&self.bcdboot_path)
                    .args([&windows_path, "/l", "zh-cn"])
                    .output()?;
                
                let stderr = gbk_to_utf8(&output.stderr);
                if !output.status.success() {
                    anyhow::bail!("Legacy 引导修复失败: {}", stderr);
                }
            }
            
            println!("[BOOT] Legacy 引导修复成功");
        }

        println!("[BOOT] ========== 引导修复完成 ==========");
        Ok(())
    }

    /// 查找 EFI 分区
    pub fn find_efi_partition(&self) -> Result<String> {
        self.find_and_mount_esp()
    }
}

impl Default for BootManager {
    fn default() -> Self {
        Self::new()
    }
}
