use crate::tr;
use crate::utils::cmd::create_command;
use anyhow::Result;
use std::path::{Path, PathBuf};

use lr_core::cached_artifact::{
    inspect_cached_artifact, verify_cached_artifact, CachedArtifactError, CachedArtifactPresence,
    CachedArtifactStatus,
};

use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::{get_bin_dir, get_exe_dir, get_pe_download_cache_dir};

fn ensure_bcdedit_success(
    arguments: &[&str],
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
            "bcdedit 执行失败（参数：{}，退出码：{}）：{}",
            arguments.join(" "),
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| tr!("未知")),
            detail
        )
    )
}

fn copy_first_boot_sdi(target: &Path, candidates: &[PathBuf]) -> Result<PathBuf> {
    for source in candidates {
        if !source.is_file() {
            continue;
        }
        let expected_size = std::fs::metadata(source)?.len();
        if expected_size == 0 {
            anyhow::bail!("{}", tr!("boot.sdi 文件为空：{}", source.display()));
        }
        let copied = std::fs::copy(source, target)?;
        let actual_size = std::fs::metadata(target)?.len();
        if copied != expected_size || actual_size != expected_size {
            anyhow::bail!(
                "{}",
                tr!(
                    "boot.sdi 复制后大小不一致：源 {} 字节，目标 {} 字节",
                    expected_size,
                    actual_size
                )
            );
        }
        return Ok(target.to_path_buf());
    }
    anyhow::bail!(
        "{}",
        tr!(
            "未找到可信的 boot.sdi，已停止创建 PE 引导；请修复当前 Windows 启动文件或使用包含 boot.sdi 的 PE ISO"
        )
    )
}

/// WinPE 启动管理器
pub struct PeManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl PeManager {
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        let bcdedit_path = lr_core::windows_compat::system_directory()
            .map(|directory| directory.join("bcdedit.exe"))
            .unwrap_or_else(|error| {
                log::error!("[PE BOOT] 无法解析宿主 System32，bcdedit 将失败关闭: {error}");
                PathBuf::from("__LetRecovery_missing_System32__").join("bcdedit.exe")
            });
        Self {
            bcdedit_path: bcdedit_path.to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    fn user_managed_directories() -> Vec<PathBuf> {
        let exe_dir = get_exe_dir();
        vec![
            get_bin_dir().join("pe"),
            exe_dir.clone(),
            exe_dir.join("PE"),
            exe_dir.join("pe"),
        ]
    }

    fn managed_cache_directories() -> Vec<PathBuf> {
        let mut directories = vec![get_pe_download_cache_dir()];
        if let Some(download_dir) = dirs::download_dir() {
            directories.push(download_dir);
        }
        directories
    }

    /// Locate a user-managed local PE or a managed downloaded PE.
    ///
    /// Files shipped in `bin/pe` intentionally remain customizable and are
    /// constrained to regular files without enforcing server metadata. Files
    /// from the managed download cache retain strict checksum verification.
    pub fn find_cached_pe(
        filename: &str,
        sha256: Option<&str>,
        md5: Option<&str>,
    ) -> std::result::Result<CachedArtifactPresence, CachedArtifactError> {
        inspect_pe_candidates(
            filename,
            &Self::user_managed_directories(),
            &Self::managed_cache_directories(),
            sha256,
            md5,
        )
    }

    /// 查找并校验缓存的 PE 文件。
    ///
    /// 文件名来自服务器配置，因此在拼接路径前必须先通过单文件名校验。
    /// 用户管理目录中的 PE 允许自定义；联网下载缓存则 SHA-256 优先于兼容字段 MD5，
    /// 声明过的校验值无法验证时会失败关闭。
    pub fn check_cached_pe(
        filename: &str,
        sha256: Option<&str>,
        md5: Option<&str>,
    ) -> std::result::Result<CachedArtifactStatus, CachedArtifactError> {
        verify_pe_candidates(
            filename,
            &Self::user_managed_directories(),
            &Self::managed_cache_directories(),
            sha256,
            md5,
        )
    }

    /// 仅检查 PE 文件是否存在的兼容接口。
    ///
    /// 高权限操作在真正使用 PE 前必须调用 `check_cached_pe` 并提供服务端校验值。
    pub fn check_pe_exists(filename: &str) -> (bool, String) {
        match Self::find_cached_pe(filename, None, None) {
            Ok(CachedArtifactPresence::Present { path, .. }) => {
                (true, path.to_string_lossy().into_owned())
            }
            Ok(CachedArtifactPresence::Missing) => (false, String::new()),
            Err(error) => {
                log::warn!("[PE] Rejected cached PE lookup for {filename:?}: {error}");
                (false, String::new())
            }
        }
    }

    /// 使用共享 WinAPI 边界检查当前 Windows 的实际固件启动模式。
    pub fn is_uefi_boot() -> Result<bool> {
        match lr_core::windows_firmware::detect_firmware_type()? {
            lr_core::windows_firmware::FirmwareType::Uefi => Ok(true),
            lr_core::windows_firmware::FirmwareType::Bios => Ok(false),
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

        let (target_wim, target_sdi) =
            crate::core::iso::IsoMounter::with_mounted_iso(iso_path, |mount_point| {
                log::info!("[PE] ISO已挂载到: {mount_point}");
                let wim_path = [
                    format!("{}\\sources\\boot.wim", mount_point),
                    format!("{}\\Boot\\boot.wim", mount_point),
                    format!("{}\\boot.wim", mount_point),
                    format!("{}\\BOOT\\BOOT.WIM", mount_point),
                ]
                .into_iter()
                .find(|path| Path::new(path).exists())
                .ok_or_else(|| anyhow::anyhow!("{}", tr!("ISO中未找到 boot.wim")))?;
                let sdi_path = [
                    format!("{}\\boot\\boot.sdi", mount_point),
                    format!("{}\\Boot\\boot.sdi", mount_point),
                    format!("{}\\BOOT\\BOOT.SDI", mount_point),
                ]
                .into_iter()
                .find(|path| Path::new(path).exists())
                .ok_or_else(|| anyhow::anyhow!("{}", tr!("ISO中未找到有效的 boot.sdi")))?;

                let target_dir = "C:\\LetRecovery_PE";
                std::fs::create_dir_all(target_dir)?;
                let target_wim = format!("{}\\boot.wim", target_dir);
                let target_sdi = format!("{}\\boot.sdi", target_dir);
                std::fs::copy(&wim_path, &target_wim)?;
                std::fs::copy(&sdi_path, &target_sdi)?;
                Ok((target_wim, target_sdi))
            })?;

        // 创建BCD引导项
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

        // 1.5 BitLocker 密钥透传：把各加密卷的恢复密钥打包进刚拷好的 boot.wim
        Self::maybe_inject_bitlocker_keys(&target_wim);

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
        let tmp = match lr_core::scoped_temp_file::ScopedTempFile::create_in(
            &std::env::temp_dir(),
            "letrecovery-bitlocker-keys",
            "json",
            text.as_bytes(),
        ) {
            Ok(tmp) => tmp,
            Err(error) => {
                log::info!("[PE] failed to create temporary BitLocker key file: {error}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&tmp, text) {
            log::info!("[PE][实验] 写临时密钥文件失败: {}（跳过注入）", e);
            return;
        }

        if let Err(error) =
            lr_core::scoped_temp_file::restrict_to_system_and_administrators(tmp.path())
        {
            log::info!("[PE] failed to restrict temporary BitLocker key file ACL: {error}");
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
            Err(e) => log::info!(
                "[PE][实验] 注入 boot.wim 失败: {}（PE 端将无法自动解锁）",
                e
            ),
        }
    }

    /// 创建默认的boot.sdi文件
    fn create_default_sdi(&self, target_dir: &str) -> Result<String> {
        let sdi_path = PathBuf::from(format!("{}\\boot.sdi", target_dir));

        // 尝试从Windows系统复制
        let system_sdi_paths = [
            PathBuf::from("C:\\Windows\\Boot\\DVD\\PCAT\\boot.sdi"),
            PathBuf::from("C:\\Windows\\Boot\\DVD\\EFI\\boot.sdi"),
        ];

        if let Some(source) = system_sdi_paths.iter().find(|path| path.is_file()) {
            log::info!("[PE] 从系统复制 boot.sdi: {}", source.display());
        }
        copy_first_boot_sdi(&sdi_path, &system_sdi_paths)
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn run_bcdedit(&self, arguments: &[&str]) -> Result<String> {
        let output = create_command(&self.bcdedit_path)
            .args(arguments)
            .output()
            .map_err(|error| {
                anyhow::anyhow!(
                    "{}",
                    tr!(
                        "无法启动 bcdedit（参数：{}）：{}",
                        arguments.join(" "),
                        error
                    )
                )
            })?;
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        ensure_bcdedit_success(
            arguments,
            output.status.success(),
            output.status.code(),
            &stdout,
            &stderr,
        )?;
        log::info!(
            "[PE] bcdedit {:?}: stdout={} stderr={}",
            arguments,
            stdout,
            stderr
        );
        Ok(stdout)
    }

    /// 创建PE引导项
    fn create_pe_boot_entry(
        &self,
        display_name: &str,
        wim_path: &str,
        sdi_path: &str,
    ) -> Result<()> {
        log::info!("[PE] 创建BCD引导项");

        let is_uefi = Self::is_uefi_boot()?;
        log::info!("[PE] 引导模式: {}", if is_uefi { "UEFI" } else { "Legacy" });

        // 清理旧的PE引导项
        self.cleanup_old_pe_entries()?;

        // 转换路径为BCD格式
        let wim_bcd_path = wim_path.replace("C:", "").replace("/", "\\");
        let sdi_bcd_path = sdi_path.replace("C:", "").replace("/", "\\");

        // 1. 创建ramdisk设备
        log::info!("[PE] 创建 ramdisk 设备");
        let ram_description = format!("{} RAM", display_name);
        let stdout = self.run_bcdedit(&["/create", "/d", &ram_description, "/device"])?;
        let ramdisk_guid = Self::extract_guid(&stdout)?;
        log::info!("[PE] Ramdisk GUID: {}", ramdisk_guid);

        // 配置ramdisk
        let cmds = [
            vec!["/set", &ramdisk_guid, "ramdisksdidevice", "partition=C:"],
            vec!["/set", &ramdisk_guid, "ramdisksdipath", &sdi_bcd_path],
        ];

        for cmd in &cmds {
            self.run_bcdedit(cmd)?;
        }

        // 2. 创建osloader
        log::info!("[PE] 创建 osloader");
        let stdout =
            self.run_bcdedit(&["/create", "/d", display_name, "/application", "osloader"])?;
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
            self.run_bcdedit(cmd)?;
        }

        // 3. 添加到启动菜单
        log::info!("[PE] 添加到启动菜单");
        self.run_bcdedit(&["/displayorder", &loader_guid, "/addfirst"])?;

        // 4. 设置超时
        self.run_bcdedit(&["/timeout", "5"])?;

        // 5. 保存GUID用于清理
        let guid_file = "C:\\LetRecovery_PE\\pe_guid.txt";
        std::fs::write(guid_file, format!("{}\n{}", ramdisk_guid, loader_guid))?;

        Ok(())
    }

    /// 设置下次启动为PE
    fn set_next_boot(&self) -> Result<()> {
        // 读取PE的loader GUID
        let guid_file = "C:\\LetRecovery_PE\\pe_guid.txt";
        let content = std::fs::read_to_string(guid_file)
            .map_err(|error| anyhow::anyhow!("读取 PE 引导 GUID 文件失败: {}", error))?;
        let loader_guid = content
            .lines()
            .nth(1)
            .filter(|guid| !guid.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("PE 引导 GUID 文件缺少 loader GUID"))?;
        log::info!("[PE] 设置下次启动: {}", loader_guid);
        self.run_bcdedit(&["/bootsequence", loader_guid])?;
        Ok(())
    }

    /// 清理旧的PE引导项
    fn cleanup_old_pe_entries(&self) -> Result<()> {
        let guid_file = "C:\\LetRecovery_PE\\pe_guid.txt";
        let content = match std::fs::read_to_string(guid_file) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for guid in content.lines() {
            if !guid.trim().is_empty() {
                log::info!("[PE] 清理旧引导项: {}", guid);
                self.run_bcdedit(&["/delete", guid, "/f"])?;
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
        if let Err(error) =
            lr_core::windows_shutdown::schedule_restart(3, "LetRecovery 正在重启到 PE 环境...")
        {
            log::error!("[PE] 安排重启失败: {error}");
        }
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

fn inspect_pe_candidates(
    filename: &str,
    user_managed_directories: &[PathBuf],
    managed_cache_directories: &[PathBuf],
    sha256: Option<&str>,
    md5: Option<&str>,
) -> std::result::Result<CachedArtifactPresence, CachedArtifactError> {
    match inspect_cached_artifact(filename, user_managed_directories, None, None)? {
        present @ CachedArtifactPresence::Present { .. } => Ok(present),
        CachedArtifactPresence::Missing => {
            inspect_cached_artifact(filename, managed_cache_directories, sha256, md5)
        }
    }
}

fn verify_pe_candidates(
    filename: &str,
    user_managed_directories: &[PathBuf],
    managed_cache_directories: &[PathBuf],
    sha256: Option<&str>,
    md5: Option<&str>,
) -> std::result::Result<CachedArtifactStatus, CachedArtifactError> {
    match verify_cached_artifact(filename, user_managed_directories, None, None)? {
        ready @ CachedArtifactStatus::Ready { .. } => Ok(ready),
        CachedArtifactStatus::Missing => {
            verify_cached_artifact(filename, managed_cache_directories, sha256, md5)
        }
    }
}

impl Default for PeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod cache_policy_tests {
    use super::*;
    use lr_core::cached_artifact::CachedArtifactVerification;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const WRONG_MD5: &str = "00000000000000000000000000000000";

    #[test]
    fn bcdedit_nonzero_exit_is_never_treated_as_success() {
        let error = ensure_bcdedit_success(
            &["/bootsequence", "{fixture}"],
            false,
            Some(5),
            "",
            "access denied",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("/bootsequence"));
        assert!(error.contains('5'));
        assert!(error.contains("access denied"));
    }

    #[test]
    fn bcdedit_zero_exit_is_accepted() {
        ensure_bcdedit_success(&["/timeout", "5"], true, Some(0), "ok", "").unwrap();
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "letrecovery-pe-policy-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create isolated test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_boot_sdi_fails_without_creating_placeholder() {
        let fixture = TestDirectory::new("missing-sdi");
        let target = fixture.0.join("boot.sdi");

        let error = copy_first_boot_sdi(&target, &[fixture.0.join("missing-source.sdi")])
            .unwrap_err()
            .to_string();

        assert!(error.contains("boot.sdi"));
        assert!(!target.exists());
    }

    #[test]
    fn boot_sdi_copy_requires_and_preserves_a_real_source() {
        let fixture = TestDirectory::new("copy-sdi");
        let source = fixture.0.join("source.sdi");
        let target = fixture.0.join("target.sdi");
        let bytes = b"trusted boot sdi fixture";
        fs::write(&source, bytes).unwrap();

        let copied = copy_first_boot_sdi(&target, std::slice::from_ref(&source)).unwrap();

        assert_eq!(copied, target);
        assert_eq!(fs::read(copied).unwrap(), bytes);
    }

    #[test]
    fn user_managed_pe_can_be_customized_without_matching_server_hash() {
        let local = TestDirectory::new("local");
        let managed = TestDirectory::new("managed-empty");
        let path = local.0.join("LetRecovery_PE.wim");
        fs::write(&path, b"custom PE contents").unwrap();

        let status = verify_pe_candidates(
            "LetRecovery_PE.wim",
            std::slice::from_ref(&local.0),
            std::slice::from_ref(&managed.0),
            None,
            Some(WRONG_MD5),
        )
        .unwrap();

        assert_eq!(
            status,
            CachedArtifactStatus::Ready {
                path,
                verification: CachedArtifactVerification::NotProvided,
            }
        );
    }

    #[test]
    fn managed_download_cache_still_fails_closed_on_hash_mismatch() {
        let local = TestDirectory::new("local-empty");
        let managed = TestDirectory::new("managed");
        let path = managed.0.join("LetRecovery_PE.wim");
        fs::write(&path, b"corrupted download").unwrap();

        let error = verify_pe_candidates(
            "LetRecovery_PE.wim",
            std::slice::from_ref(&local.0),
            std::slice::from_ref(&managed.0),
            None,
            Some(WRONG_MD5),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CachedArtifactError::HashMismatch { path: failed, .. } if failed == path
        ));
    }
}
