use anyhow::Result;
use std::path::Path;
use crate::utils::cmd::create_command;

use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::{get_bin_dir, get_exe_dir};

/// WinPE 启动管理器
pub struct PeManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl PeManager {
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        Self {
            bcdedit_path: bin_dir.join("bcdedit.exe").to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    /// 检查PE文件是否存在
    /// 返回 (存在, 完整路径)
    pub fn check_pe_exists(filename: &str) -> (bool, String) {
        // 检查多个可能的位置
        let locations = [
            get_exe_dir().join(filename),
            get_exe_dir().join("PE").join(filename),
            get_exe_dir().join("pe").join(filename),
            dirs::download_dir().unwrap_or_default().join(filename),
        ];

        for path in &locations {
            if path.exists() {
                return (true, path.to_string_lossy().to_string());
            }
        }

        (false, String::new())
    }

    /// 检查是否为UEFI启动
    pub fn is_uefi_boot() -> bool {
        // 检查 EFI 系统分区是否存在
        Path::new("C:\\Windows\\Boot\\EFI").exists()
            || std::env::var("firmware_type")
                .map(|v| v.to_lowercase() == "uefi")
                .unwrap_or(false)
            || {
                // 通过 bcdedit 检查
                let output = create_command("bcdedit")
                    .args(["/enum", "{current}"])
                    .output();
                if let Ok(out) = output {
                    let stdout = gbk_to_utf8(&out.stdout);
                    stdout.contains("winload.efi")
                } else {
                    false
                }
            }
    }

    /// 从ISO/WIM启动PE
    /// pe_path: PE文件路径 (.iso 或 .wim)
    /// display_name: 显示名称
    pub fn boot_to_pe(&self, pe_path: &str, display_name: &str) -> Result<()> {
        println!("[PE] ========== 准备启动 PE ==========");
        println!("[PE] PE文件: {}", pe_path);
        println!("[PE] 显示名称: {}", display_name);

        let pe_path_lower = pe_path.to_lowercase();
        
        if pe_path_lower.ends_with(".iso") {
            self.boot_from_iso(pe_path, display_name)
        } else if pe_path_lower.ends_with(".wim") {
            self.boot_from_wim(pe_path, display_name)
        } else {
            anyhow::bail!("不支持的PE文件格式，请使用 .iso 或 .wim 文件")
        }
    }

    /// 从ISO启动PE
    fn boot_from_iso(&self, iso_path: &str, display_name: &str) -> Result<()> {
        println!("[PE] 从ISO启动PE");
        
        // 1. 挂载ISO
        crate::core::iso::IsoMounter::mount_iso(iso_path)?;
        let mount_point = crate::core::iso::IsoMounter::find_iso_drive()
            .ok_or_else(|| anyhow::anyhow!("无法找到ISO挂载点"))?;
        println!("[PE] ISO已挂载到: {}", mount_point);

        // 2. 查找PE WIM文件
        let wim_paths = [
            format!("{}\\sources\\boot.wim", mount_point),
            format!("{}\\Boot\\boot.wim", mount_point),
            format!("{}\\boot.wim", mount_point),
            format!("{}\\BOOT\\BOOT.WIM", mount_point),
        ];

        let mut wim_path = None;
        for path in &wim_paths {
            if Path::new(path).exists() {
                wim_path = Some(path.clone());
                break;
            }
        }

        let wim_path = wim_path.ok_or_else(|| anyhow::anyhow!("ISO中未找到 boot.wim"))?;
        println!("[PE] 找到WIM: {}", wim_path);

        // 3. 查找boot.sdi
        let sdi_paths = [
            format!("{}\\boot\\boot.sdi", mount_point),
            format!("{}\\Boot\\boot.sdi", mount_point),
            format!("{}\\BOOT\\BOOT.SDI", mount_point),
        ];

        let mut sdi_path = None;
        for path in &sdi_paths {
            if Path::new(path).exists() {
                sdi_path = Some(path.clone());
                break;
            }
        }

        // 4. 复制必要文件到系统分区
        let target_dir = "C:\\LetRecovery_PE";
        std::fs::create_dir_all(target_dir)?;

        let target_wim = format!("{}\\boot.wim", target_dir);
        println!("[PE] 复制 boot.wim 到 {}", target_wim);
        std::fs::copy(&wim_path, &target_wim)?;

        let target_sdi = if let Some(sdi) = sdi_path {
            let target = format!("{}\\boot.sdi", target_dir);
            println!("[PE] 复制 boot.sdi 到 {}", target);
            std::fs::copy(&sdi, &target)?;
            target
        } else {
            // 创建默认的boot.sdi
            self.create_default_sdi(target_dir)?
        };

        // 5. 卸载ISO
        let _ = crate::core::iso::IsoMounter::unmount();

        // 6. 创建BCD引导项
        self.create_pe_boot_entry(display_name, &target_wim, &target_sdi)?;

        // 7. 设置下次启动
        self.set_next_boot()?;

        println!("[PE] ========== PE启动准备完成 ==========");
        Ok(())
    }

    /// 从WIM直接启动PE
    fn boot_from_wim(&self, wim_path: &str, display_name: &str) -> Result<()> {
        println!("[PE] 从WIM启动PE");

        // 1. 复制WIM到系统分区
        let target_dir = "C:\\LetRecovery_PE";
        std::fs::create_dir_all(target_dir)?;

        let target_wim = format!("{}\\boot.wim", target_dir);
        println!("[PE] 复制 WIM 到 {}", target_wim);
        std::fs::copy(wim_path, &target_wim)?;

        // 2. 创建或使用boot.sdi
        let target_sdi = self.create_default_sdi(target_dir)?;

        // 3. 创建BCD引导项
        self.create_pe_boot_entry(display_name, &target_wim, &target_sdi)?;

        // 4. 设置下次启动
        self.set_next_boot()?;

        println!("[PE] ========== PE启动准备完成 ==========");
        Ok(())
    }

    /// 创建默认的boot.sdi文件
    fn create_default_sdi(&self, target_dir: &str) -> Result<String> {
        let sdi_path = format!("{}\\boot.sdi", target_dir);
        
        // 尝试从Windows系统复制
        let system_sdi_paths = [
            "C:\\Windows\\Boot\\DVD\\PCAT\\boot.sdi",
            "C:\\Windows\\Boot\\DVD\\EFI\\boot.sdi",
        ];

        for path in &system_sdi_paths {
            if Path::new(path).exists() {
                println!("[PE] 从系统复制 boot.sdi: {}", path);
                std::fs::copy(path, &sdi_path)?;
                return Ok(sdi_path);
            }
        }

        // 如果系统中没有，创建一个空的SDI文件（最小有效SDI）
        // SDI文件头结构
        println!("[PE] 创建最小 boot.sdi");
        let sdi_header: [u8; 512] = {
            let mut header = [0u8; 512];
            // SDI signature: "$SDI"
            header[0] = b'$';
            header[1] = b'S';
            header[2] = b'D';
            header[3] = b'I';
            // Version
            header[4] = 0x01;
            header[5] = 0x00;
            header[6] = 0x01;
            header[7] = 0x00;
            header
        };
        std::fs::write(&sdi_path, &sdi_header)?;

        Ok(sdi_path)
    }

    /// 创建PE引导项
    fn create_pe_boot_entry(&self, display_name: &str, wim_path: &str, sdi_path: &str) -> Result<()> {
        println!("[PE] 创建BCD引导项");
        
        let is_uefi = Self::is_uefi_boot();
        println!("[PE] 引导模式: {}", if is_uefi { "UEFI" } else { "Legacy" });

        // 清理旧的PE引导项
        let _ = self.cleanup_old_pe_entries();

        // 转换路径为BCD格式
        let wim_bcd_path = wim_path.replace("C:", "").replace("/", "\\");
        let sdi_bcd_path = sdi_path.replace("C:", "").replace("/", "\\");

        // 1. 创建ramdisk设备
        println!("[PE] 创建 ramdisk 设备");
        let output = create_command(&self.bcdedit_path)
            .args(["/create", "/d", &format!("{} RAM", display_name), "/device"])
            .output()?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        println!("[PE] bcdedit output: {}", stdout);
        let ramdisk_guid = Self::extract_guid(&stdout)?;
        println!("[PE] Ramdisk GUID: {}", ramdisk_guid);

        // 配置ramdisk
        let cmds = [
            vec!["/set", &ramdisk_guid, "ramdisksdidevice", "partition=C:"],
            vec!["/set", &ramdisk_guid, "ramdisksdipath", &sdi_bcd_path],
        ];

        for cmd in &cmds {
            let output = create_command(&self.bcdedit_path).args(cmd).output()?;
            println!("[PE] bcdedit {:?}: {}", cmd, gbk_to_utf8(&output.stdout));
        }

        // 2. 创建osloader
        println!("[PE] 创建 osloader");
        let output = create_command(&self.bcdedit_path)
            .args(["/create", "/d", display_name, "/application", "osloader"])
            .output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        println!("[PE] bcdedit output: {}", stdout);
        let loader_guid = Self::extract_guid(&stdout)?;
        println!("[PE] Loader GUID: {}", loader_guid);

        // 配置osloader
        let winload = if is_uefi {
            "\\windows\\system32\\boot\\winload.efi"
        } else {
            "\\windows\\system32\\boot\\winload.exe"
        };

        let device_str = format!("ramdisk=[C:]{},{}", wim_bcd_path, ramdisk_guid);
        
        let cmds = [
            vec!["/set", &loader_guid, "device", &device_str],
            vec!["/set", &loader_guid, "path", winload],
            vec!["/set", &loader_guid, "osdevice", &device_str],
            vec!["/set", &loader_guid, "systemroot", "\\windows"],
            vec!["/set", &loader_guid, "detecthal", "yes"],
            vec!["/set", &loader_guid, "winpe", "yes"],
            vec!["/set", &loader_guid, "ems", "no"],
        ];

        for cmd in &cmds {
            let output = create_command(&self.bcdedit_path).args(cmd).output()?;
            let out_str = gbk_to_utf8(&output.stdout);
            let err_str = gbk_to_utf8(&output.stderr);
            println!("[PE] bcdedit {:?}: {} {}", cmd, out_str, err_str);
        }

        // 3. 添加到启动菜单
        println!("[PE] 添加到启动菜单");
        let output = create_command(&self.bcdedit_path)
            .args(["/displayorder", &loader_guid, "/addfirst"])
            .output()?;
        println!("[PE] displayorder: {}", gbk_to_utf8(&output.stdout));

        // 4. 设置超时
        let output = create_command(&self.bcdedit_path)
            .args(["/timeout", "5"])
            .output()?;
        println!("[PE] timeout: {}", gbk_to_utf8(&output.stdout));

        // 5. 保存GUID用于清理
        let guid_file = "C:\\LetRecovery_PE\\pe_guid.txt";
        std::fs::write(guid_file, format!("{}\n{}", ramdisk_guid, loader_guid))?;

        Ok(())
    }

    /// 设置下次启动为PE
    fn set_next_boot(&self) -> Result<()> {
        // 读取PE的loader GUID
        let guid_file = "C:\\LetRecovery_PE\\pe_guid.txt";
        if let Ok(content) = std::fs::read_to_string(guid_file) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() >= 2 {
                let loader_guid = lines[1];
                println!("[PE] 设置下次启动: {}", loader_guid);
                
                let output = create_command(&self.bcdedit_path)
                    .args(["/bootsequence", loader_guid])
                    .output()?;
                println!("[PE] bootsequence: {}", gbk_to_utf8(&output.stdout));
            }
        }
        Ok(())
    }

    /// 清理旧的PE引导项
    fn cleanup_old_pe_entries(&self) -> Result<()> {
        let guid_file = "C:\\LetRecovery_PE\\pe_guid.txt";
        if let Ok(content) = std::fs::read_to_string(guid_file) {
            for guid in content.lines() {
                if !guid.is_empty() {
                    println!("[PE] 清理旧引导项: {}", guid);
                    let _ = create_command(&self.bcdedit_path)
                        .args(["/delete", guid, "/f"])
                        .output();
                }
            }
        }
        Ok(())
    }

    /// 清理PE文件和引导项
    pub fn cleanup_pe(&self) -> Result<()> {
        println!("[PE] 清理PE");
        
        // 清理BCD引导项
        self.cleanup_old_pe_entries()?;

        // 删除PE文件
        let pe_dir = "C:\\LetRecovery_PE";
        if Path::new(pe_dir).exists() {
            let _ = std::fs::remove_dir_all(pe_dir);
        }

        Ok(())
    }

    /// 重启系统
    pub fn reboot() {
        println!("[PE] 执行重启");
        let _ = create_command("shutdown")
            .args(["/r", "/t", "3", "/c", "LetRecovery 正在重启到 PE 环境..."])
            .spawn();
    }

    /// 从bcdedit输出中提取GUID
    fn extract_guid(output: &str) -> Result<String> {
        for word in output.split_whitespace() {
            if word.starts_with('{') && word.ends_with('}') {
                return Ok(word.to_string());
            }
            if word.starts_with('{') {
                let cleaned: String = word
                    .chars()
                    .filter(|c| !c.is_ascii_punctuation() || *c == '-' || *c == '{' || *c == '}')
                    .collect();
                if cleaned.ends_with('}') && cleaned.len() > 10 {
                    return Ok(cleaned);
                }
            }
        }
        
        // 尝试用正则匹配
        for line in output.lines() {
            if let Some(start) = line.find('{') {
                if let Some(end) = line[start..].find('}') {
                    let guid = &line[start..start + end + 1];
                    if guid.len() > 10 {
                        return Ok(guid.to_string());
                    }
                }
            }
        }
        
        anyhow::bail!("无法从bcdedit输出中提取GUID: {}", output)
    }
}

impl Default for PeManager {
    fn default() -> Self {
        Self::new()
    }
}
