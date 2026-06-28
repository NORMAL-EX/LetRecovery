use anyhow::Result;
use std::path::Path;
use crate::tr;
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
        // 检查多个可能的位置（新布局优先：bin\pe）
        let locations = [
            get_bin_dir().join("pe").join(filename),
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
        log::info!("[PE] ========== 准备启动 PE ==========");
        log::info!("[PE] PE文件: {}", pe_path);
        log::info!("[PE] 显示名称: {}", display_name);

        let pe_path_lower = pe_path.to_lowercase();
        
        if pe_path_lower.ends_with(".iso") {
            self.boot_from_iso(pe_path, display_name)
        } else if pe_path_lower.ends_with(".wim") {
            self.boot_from_wim(pe_path, display_name)
        } else {
            anyhow::bail!("{}", tr!("不支持的PE文件格式，请使用 .iso 或 .wim 文件"))
        }
    }

    /// 从ISO启动PE
    fn boot_from_iso(&self, iso_path: &str, display_name: &str) -> Result<()> {
        log::info!("[PE] 从ISO启动PE");
        
        // 1. 挂载ISO
        crate::core::iso::IsoMounter::mount_iso(iso_path)?;
        let mount_point = crate::core::iso::IsoMounter::find_iso_drive()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法找到ISO挂载点")))?;
        log::info!("[PE] ISO已挂载到: {}", mount_point);

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

        let wim_path = wim_path.ok_or_else(|| anyhow::anyhow!("{}", tr!("ISO中未找到 boot.wim")))?;
        log::info!("[PE] 找到WIM: {}", wim_path);

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
        log::info!("[PE] 复制 boot.wim 到 {}", target_wim);
        std::fs::copy(&wim_path, &target_wim)?;

        let target_sdi = if let Some(sdi) = sdi_path {
            let target = format!("{}\\boot.sdi", target_dir);
            log::info!("[PE] 复制 boot.sdi 到 {}", target);
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

        log::info!("[PE] ========== PE启动准备完成 ==========");
        Ok(())
    }

    /// 从WIM直接启动PE
    fn boot_from_wim(&self, wim_path: &str, display_name: &str) -> Result<()> {
        log::info!("[PE] 从WIM启动PE");

        // 1. 复制WIM到系统分区
        let target_dir = "C:\\LetRecovery_PE";
        std::fs::create_dir_all(target_dir)?;

        let target_wim = format!("{}\\boot.wim", target_dir);
        log::info!("[PE] 复制 WIM 到 {}", target_wim);
        std::fs::copy(wim_path, &target_wim)?;

        // 1.5 【实验性】BitLocker 密钥透传：把各加密卷的恢复密钥打包进刚拷好的 boot.wim
        Self::maybe_inject_bitlocker_keys(&target_wim);

        // 1.6 Secure Boot：PE 自带的 winload.efi 仅 2011 链签名，在已吊销 2011(CVE-2023-24932 DBX)
        //     或仅信任 2023 证书的机器上会被 Secure Boot 拦下。若当前系统的 winload 是双签名(2011+2023)
        //     且与 PE 同内核家族，则用它覆盖 PE 内的 winload，使 PE 在新老/已吊销机器上都能过 Secure Boot。
        Self::maybe_upgrade_pe_bootloader(&target_wim);

        // 2. 创建或使用boot.sdi
        let target_sdi = self.create_default_sdi(target_dir)?;

        // 3. 创建BCD引导项
        self.create_pe_boot_entry(display_name, &target_wim, &target_sdi)?;

        // 4. 设置下次启动
        self.set_next_boot()?;

        log::info!("[PE] ========== PE启动准备完成 ==========");
        Ok(())
    }

    /// 抓取各 BitLocker 加密卷的恢复密钥，打包进刚拷好的 PE boot.wim
    /// （镜像 1，路径见 `lr_core::bl_passthrough::KEYS_WIM_PATH`）。
    ///
    /// BitLocker 密钥透传现为默认行为（无开关）：能拿到目标盘密钥时即走透传，由 PE 启动后
    /// 用恢复密钥解锁再部署；拿不到目标盘密钥时正常端已回退"彻底解密"方案，那条路径下进到
    /// 这里时各卷已解密，`get_encrypted_volumes` 返回空 → 本函数自然空操作。
    ///
    /// 全程 best-effort：无加密卷、取不到恢复密钥、或注入失败都只记录日志，
    /// 绝不影响 PE 启动流程本身。临时密钥文件用后即删（密钥仅随 boot.wim 进入 PE 的内存盘）。
    fn maybe_inject_bitlocker_keys(target_wim: &str) {
        log::info!("[PE] BitLocker 密钥透传：抓取各加密卷恢复密钥…");

        let manager = crate::core::bitlocker::BitLockerManager::new();
        let volumes = manager.get_encrypted_volumes(); // 仅返回已加密卷
        if volumes.is_empty() {
            log::info!("[PE] 未发现 BitLocker 加密卷，跳过密钥注入");
            return;
        }
        let mut entries: Vec<(String, String)> = Vec::new();
        for v in &volumes {
            match manager.get_recovery_key(&v.letter) {
                Ok(key) => {
                    log::info!("[PE][实验] 已取恢复密钥: {} {}", v.letter, v.label);
                    entries.push((format!("{} {}", v.letter, v.label), key));
                }
                Err(e) => {
                    log::info!("[PE][实验] 取恢复密钥失败 {}: {}（跳过该卷）", v.letter, e);
                }
            }
        }
        if entries.is_empty() {
            log::info!("[PE][实验] 未取得任何 BitLocker 恢复密钥，跳过注入");
            return;
        }

        let text = lr_core::bl_passthrough::serialize_keys(&entries);
        let tmp = std::env::temp_dir().join(lr_core::bl_passthrough::KEYS_FILE_NAME);
        if let Err(e) = std::fs::write(&tmp, text) {
            log::info!("[PE][实验] 写临时密钥文件失败: {}（跳过注入）", e);
            return;
        }

        let inject = (|| -> Result<()> {
            let mgr = lr_core::wimlib::WimlibManager::new().map_err(|e| anyhow::anyhow!(e))?;
            mgr.add_file_to_image(
                target_wim,
                1,
                &tmp.to_string_lossy(),
                lr_core::bl_passthrough::KEYS_WIM_PATH,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp); // 密钥不留盘
        match inject {
            Ok(()) => log::info!(
                "[PE][实验] 已把 {} 个卷的恢复密钥注入 boot.wim",
                entries.len()
            ),
            Err(e) => log::info!("[PE][实验] 注入 boot.wim 失败: {}（PE 端将无法自动解锁）", e),
        }
    }

    /// 当前打包 PE 的内核家族（winload 版本前缀）。换 PE 基线时同步更新此常量。
    /// 仅当“当前系统”的 winload 属于同一家族时才允许用它覆盖 PE 的 winload，避免 winload/内核版本不兼容。
    const PE_WINLOAD_FAMILY: &'static str = "10.0.19041.";

    /// 若满足条件，用【当前系统】的 winload.efi 覆盖 PE boot.wim 内的 winload.efi，
    /// 让 PE 在“仅信任 2023 / 已吊销 2011(CVE-2023-24932 DBX)”的 Secure Boot 机器上也能启动。
    ///
    /// 条件（任一不满足即原样保留 PE 自带的 winload，绝不降级、不冒险）：
    /// 1. 当前已开启 Secure Boot（否则用不着，避免无谓改动启动链）；
    /// 2. 当前系统 winload.efi 含 2023 证书（双签名 2011+2023，新老机器都能过）；
    /// 3. 当前系统 winload.efi 与 PE 同属 `PE_WINLOAD_FAMILY` 内核家族（版本兼容）。
    ///
    /// best-effort：失败只记日志，不影响 PE 启动准备。PE 经目标机自带 bootmgfw 引导，
    /// 它唯一受 Secure Boot 校验的组件就是 boot.wim 内的 winload.efi，故只需替换它。
    fn maybe_upgrade_pe_bootloader(target_wim: &str) {
        // 1. 仅在 Secure Boot 开启时处理
        if !Self::is_secure_boot_enabled() {
            log::info!("[PE][SB] 未开启 Secure Boot，无需升级 PE winload");
            return;
        }

        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let host_winload = format!("{}\\System32\\winload.efi", sysroot);
        let bytes = match std::fs::read(&host_winload) {
            Ok(b) => b,
            Err(e) => {
                log::info!("[PE][SB] 读取当前系统 winload.efi 失败: {}，保留 PE 原 winload", e);
                return;
            }
        };

        // 2. 当前系统 winload 是否含 2023 证书（双签名 / 2023 签名）
        let has_2023 = Self::bytes_contains(&bytes, b"Windows UEFI CA 2023")
            || Self::bytes_contains(&bytes, b"Microsoft Windows Production PCA 2023");
        if !has_2023 {
            log::info!("[PE][SB] 当前系统 winload 未含 2023 证书（非双签名），保留 PE 原 winload");
            return;
        }

        // 3. 内核家族匹配（winload 版本资源为 UTF-16LE，需以 UTF-16 形式匹配 "10.0.19041."）
        let fam_u16: Vec<u8> = Self::PE_WINLOAD_FAMILY
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        if !Self::bytes_contains(&bytes, &fam_u16) {
            log::info!(
                "[PE][SB] 当前系统 winload 与 PE 内核家族({})不匹配，避免版本不兼容，保留 PE 原 winload",
                Self::PE_WINLOAD_FAMILY
            );
            return;
        }

        // 4. 覆盖 PE boot.wim 内的 winload（ramdisk 引导用 \Windows\System32\Boot\winload.efi；
        //    一并覆盖 \Windows\System32\winload.efi 以防万一）。wimlib ADD 对已存在路径即为替换。
        let mgr = match lr_core::wimlib::WimlibManager::new() {
            Ok(m) => m,
            Err(e) => {
                log::info!("[PE][SB] wimlib 初始化失败: {}，保留 PE 原 winload", e);
                return;
            }
        };
        let mut ok = 0;
        for dest in [
            "\\Windows\\System32\\Boot\\winload.efi",
            "\\Windows\\System32\\winload.efi",
        ] {
            match mgr.add_file_to_image(target_wim, 1, &host_winload, dest) {
                Ok(()) => {
                    ok += 1;
                    log::info!("[PE][SB] 已用当前系统双签名 winload 覆盖 PE: {}", dest);
                }
                Err(e) => log::info!("[PE][SB] 覆盖 {} 失败: {}", dest, e),
            }
        }
        if ok > 0 {
            log::info!("[PE][SB] PE winload 已升级为双签名(2011+2023)，可在已吊销2011/仅2023机器上过 Secure Boot");
        }
    }

    /// 内存子串查找（用于在 PE 二进制里探测证书 CN / 版本字符串）。
    fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || haystack.len() < needle.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// 当前系统是否已开启 UEFI Secure Boot（读注册表 State\UEFISecureBootEnabled）。
    fn is_secure_boot_enabled() -> bool {
        let out = crate::utils::cmd::run_command_string(
            "reg",
            &[
                "query",
                r"HKLM\SYSTEM\CurrentControlSet\Control\SecureBoot\State",
                "/v",
                "UEFISecureBootEnabled",
            ],
        );
        match out {
            Ok(s) => s.to_lowercase().contains("0x1"),
            Err(_) => false,
        }
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
                log::info!("[PE] 从系统复制 boot.sdi: {}", path);
                std::fs::copy(path, &sdi_path)?;
                return Ok(sdi_path);
            }
        }

        // 如果系统中没有，创建一个空的SDI文件（最小有效SDI）
        // SDI文件头结构
        log::info!("[PE] 创建最小 boot.sdi");
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
        log::info!("[PE] 创建BCD引导项");
        
        let is_uefi = Self::is_uefi_boot();
        log::info!("[PE] 引导模式: {}", if is_uefi { "UEFI" } else { "Legacy" });

        // 清理旧的PE引导项
        let _ = self.cleanup_old_pe_entries();

        // 转换路径为BCD格式
        let wim_bcd_path = wim_path.replace("C:", "").replace("/", "\\");
        let sdi_bcd_path = sdi_path.replace("C:", "").replace("/", "\\");

        // 1. 创建ramdisk设备
        log::info!("[PE] 创建 ramdisk 设备");
        let output = create_command(&self.bcdedit_path)
            .args(["/create", "/d", &format!("{} RAM", display_name), "/device"])
            .output()?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        log::info!("[PE] bcdedit output: {}", stdout);
        let ramdisk_guid = Self::extract_guid(&stdout)?;
        log::info!("[PE] Ramdisk GUID: {}", ramdisk_guid);

        // 配置ramdisk
        let cmds = [
            vec!["/set", &ramdisk_guid, "ramdisksdidevice", "partition=C:"],
            vec!["/set", &ramdisk_guid, "ramdisksdipath", &sdi_bcd_path],
        ];

        for cmd in &cmds {
            let output = create_command(&self.bcdedit_path).args(cmd).output()?;
            log::info!("[PE] bcdedit {:?}: {}", cmd, gbk_to_utf8(&output.stdout));
        }

        // 2. 创建osloader
        log::info!("[PE] 创建 osloader");
        let output = create_command(&self.bcdedit_path)
            .args(["/create", "/d", display_name, "/application", "osloader"])
            .output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        log::info!("[PE] bcdedit output: {}", stdout);
        let loader_guid = Self::extract_guid(&stdout)?;
        log::info!("[PE] Loader GUID: {}", loader_guid);

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
            log::info!("[PE] bcdedit {:?}: {} {}", cmd, out_str, err_str);
        }

        // 3. 添加到启动菜单
        log::info!("[PE] 添加到启动菜单");
        let output = create_command(&self.bcdedit_path)
            .args(["/displayorder", &loader_guid, "/addfirst"])
            .output()?;
        log::info!("[PE] displayorder: {}", gbk_to_utf8(&output.stdout));

        // 4. 设置超时
        let output = create_command(&self.bcdedit_path)
            .args(["/timeout", "5"])
            .output()?;
        log::info!("[PE] timeout: {}", gbk_to_utf8(&output.stdout));

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
                log::info!("[PE] 设置下次启动: {}", loader_guid);
                
                let output = create_command(&self.bcdedit_path)
                    .args(["/bootsequence", loader_guid])
                    .output()?;
                log::info!("[PE] bootsequence: {}", gbk_to_utf8(&output.stdout));
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
                    log::info!("[PE] 清理旧引导项: {}", guid);
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
        log::info!("[PE] 清理PE");
        
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
        log::info!("[PE] 执行重启");
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
        
        anyhow::bail!("{}", tr!("无法从bcdedit输出中提取GUID: {}", output))
    }
}

impl Default for PeManager {
    fn default() -> Self {
        Self::new()
    }
}
