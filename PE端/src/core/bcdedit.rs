use anyhow::Result;
use std::path::Path;
use std::{fs, path::PathBuf};

use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

pub struct BootManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl BootManager {
    /// 选择一个可靠的临时目录并确保它存在（避免 WinPE 下 os error 3）。
    fn reliable_temp_dir() -> PathBuf {
        let candidates = [
            PathBuf::from(r"X:\Windows\Temp"),
            PathBuf::from(r"X:\Temp"),
            std::env::temp_dir(),
            PathBuf::from("X:\\"),
        ];

        for dir in candidates {
            let _ = fs::create_dir_all(&dir);
            if dir.exists() {
                return dir;
            }
        }

        std::env::temp_dir()
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

        let script1_path = Self::reliable_temp_dir().join("find_disk.txt");
        std::fs::write(&script1_path, &script1)?;

        let output = new_command("diskpart")
            .args(["/s", &script1_path.to_string_lossy()])
            .output()?;

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

        let disk_num = disk_num.ok_or_else(|| anyhow::anyhow!("无法确定分区所在磁盘"))?;
        log::info!("目标分区在磁盘 {}", disk_num);

        // Step 2: 查找该磁盘上的 ESP 分区
        let script2 = format!(
            r#"select disk {}
list partition
"#,
            disk_num
        );

        let script2_path = Self::reliable_temp_dir().join("list_part.txt");
        std::fs::write(&script2_path, &script2)?;

        let output = new_command("diskpart")
            .args(["/s", &script2_path.to_string_lossy()])
            .output()?;

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

        let esp_partition = esp_partition.ok_or_else(|| anyhow::anyhow!("未找到 ESP 分区"))?;

        // Step 3: 为 ESP 分配盘符
        let _ = new_command("mountvol").args(["S:", "/d"]).output();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let script3 = format!(
            r#"select disk {}
select partition {}
assign letter=S
"#,
            disk_num, esp_partition
        );

        let script3_path = Self::reliable_temp_dir().join("assign_esp.txt");
        std::fs::write(&script3_path, &script3)?;

        let output = new_command("diskpart")
            .args(["/s", &script3_path.to_string_lossy()])
            .output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        log::debug!("分配 ESP 盘符:\n{}", stdout);

        std::thread::sleep(std::time::Duration::from_millis(500));

        if Path::new("S:\\").exists() {
            log::info!("ESP 已挂载到 S:");
            Ok("S:".to_string())
        } else {
            anyhow::bail!("ESP 盘符分配失败")
        }
    }

    /// 查找并挂载 EFI 系统分区
    pub fn find_and_mount_esp(&self) -> Result<String> {
        log::info!("查找 EFI 系统分区...");

        // 方法1: 检查 S: 是否已经是 ESP
        if Path::new("S:\\EFI").exists() {
            log::info!("S: 已经是 ESP");
            return Ok("S:".to_string());
        }

        // 方法2: 使用 mountvol /s 挂载 ESP 到 S:
        log::info!("尝试使用 mountvol /s 挂载 ESP");
        let output = new_command("mountvol").args(["S:", "/s"]).output();
        if output.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if Path::new("S:\\").exists() {
                log::info!("ESP 已通过 mountvol 挂载到 S:");
                return Ok("S:".to_string());
            }
        }

        // 方法3: 使用 diskpart 查找所有磁盘的 ESP
        self.find_esp_with_diskpart()
    }

    fn find_esp_with_diskpart(&self) -> Result<String> {
        log::info!("使用 diskpart 查找 ESP");

        for disk in 0..4 {
            let script = format!(
                r#"select disk {}
list partition
"#,
                disk
            );

            let script_path = Self::reliable_temp_dir().join("check_disk.txt");
            std::fs::write(&script_path, &script)?;

            let output = new_command("diskpart")
                .args(["/s", script_path.to_str().unwrap()])
                .output()?;

            let stdout = gbk_to_utf8(&output.stdout);

            for line in stdout.lines() {
                let line_lower = line.to_lowercase();
                if line_lower.contains("system") || line_lower.contains("系统") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    for (i, part) in parts.iter().enumerate() {
                        if part.to_lowercase().contains("partition") || *part == "分区" {
                            if let Some(num_str) = parts.get(i + 1) {
                                if let Ok(part_num) = num_str.parse::<usize>() {
                                    let assign_script = format!(
                                        r#"select disk {}
select partition {}
assign letter=S
"#,
                                        disk, part_num
                                    );

                                    let assign_path =
                                        Self::reliable_temp_dir().join("assign_esp2.txt");
                                    std::fs::write(&assign_path, &assign_script)?;

                                    let _ = new_command("diskpart")
                                        .args(["/s", &assign_path.to_string_lossy()])
                                        .output();

                                    std::thread::sleep(std::time::Duration::from_millis(500));

                                    if Path::new("S:\\").exists() {
                                        log::info!("找到 ESP: 磁盘 {} 分区 {}", disk, part_num);
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
    pub fn repair_boot_advanced(&self, windows_partition: &str, use_uefi: bool) -> Result<()> {
        let windows_path = format!("{}\\Windows", windows_partition);

        log::info!("========== 修复引导 ==========");
        log::info!("Windows 路径: {}", windows_path);
        log::info!(
            "引导模式: {}",
            if use_uefi { "UEFI" } else { "Legacy/BIOS" }
        );

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("Windows 目录不存在: {}", windows_path);
        }

        // 先删除当前PE引导项
        let _ = self.delete_current_boot_entry();

        if use_uefi {
            log::info!("UEFI 模式：查找 ESP 分区");

            let esp_result = self
                .find_esp_on_same_disk(windows_partition)
                .or_else(|_| self.find_and_mount_esp());

            match esp_result {
                Ok(esp_letter) => {
                    log::info!("ESP 分区: {}", esp_letter);

                    let efi_ms_dir = format!("{}\\EFI\\Microsoft", esp_letter);
                    let efi_boot_dir = format!("{}\\EFI\\Boot", esp_letter);

                    let _ = std::fs::create_dir_all(&efi_ms_dir);
                    let _ = std::fs::create_dir_all(&efi_boot_dir);

                    log::info!(
                        "执行: bcdboot {} /s {} /f UEFI /l zh-cn",
                        windows_path,
                        esp_letter
                    );
                    let output = new_command(&self.bcdboot_path)
                        .args([
                            &windows_path,
                            "/s",
                            &esp_letter,
                            "/f",
                            "UEFI",
                            "/l",
                            "zh-cn",
                        ])
                        .output()?;

                    let stdout = gbk_to_utf8(&output.stdout);
                    let stderr = gbk_to_utf8(&output.stderr);

                    log::debug!("bcdboot stdout: {}", stdout);
                    log::debug!("bcdboot stderr: {}", stderr);

                    if !output.status.success() {
                        log::info!("重试：使用 ALL 模式");
                        let output = new_command(&self.bcdboot_path)
                            .args([
                                &windows_path,
                                "/s",
                                &esp_letter,
                                "/f",
                                "ALL",
                                "/l",
                                "zh-cn",
                            ])
                            .output()?;

                        let stdout = gbk_to_utf8(&output.stdout);
                        let stderr = gbk_to_utf8(&output.stderr);
                        log::debug!("bcdboot (ALL) stdout: {}", stdout);
                        log::debug!("bcdboot (ALL) stderr: {}", stderr);

                        if !output.status.success() {
                            log::info!("重试：不指定引导类型");
                            let output = new_command(&self.bcdboot_path)
                                .args([&windows_path, "/s", &esp_letter, "/l", "zh-cn"])
                                .output()?;

                            let stderr = gbk_to_utf8(&output.stderr);
                            if !output.status.success() {
                                anyhow::bail!("UEFI 引导修复失败: {}", stderr);
                            }
                        }
                    }

                    // 验证引导文件
                    let bootmgfw = format!("{}\\EFI\\Microsoft\\Boot\\bootmgfw.efi", esp_letter);
                    let bootx64 = format!("{}\\EFI\\Boot\\bootx64.efi", esp_letter);

                    if Path::new(&bootmgfw).exists() {
                        log::info!("引导文件已创建: {}", bootmgfw);
                    }

                    if !Path::new(&bootx64).exists() {
                        if Path::new(&bootmgfw).exists() {
                            let _ = std::fs::copy(&bootmgfw, &bootx64);
                            log::info!("已复制 bootmgfw.efi -> bootx64.efi");
                        }
                    }

                    log::info!("UEFI 引导修复成功");
                }
                Err(e) => {
                    log::warn!("查找 ESP 失败: {}，尝试默认方式", e);

                    let output = new_command(&self.bcdboot_path)
                        .args([&windows_path, "/f", "UEFI", "/l", "zh-cn"])
                        .output()?;

                    let stdout = gbk_to_utf8(&output.stdout);
                    let stderr = gbk_to_utf8(&output.stderr);
                    log::debug!("bcdboot (auto) stdout: {}", stdout);
                    log::debug!("bcdboot (auto) stderr: {}", stderr);

                    if !output.status.success() {
                        anyhow::bail!("引导修复失败: {}", stderr);
                    }
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
                    anyhow::bail!("Legacy 引导修复失败: {}", stderr);
                }
            }

            log::info!("Legacy 引导修复成功");
        }

        log::info!("========== 引导修复完成 ==========");
        Ok(())
    }
}

impl Default for BootManager {
    fn default() -> Self {
        Self::new()
    }
}
