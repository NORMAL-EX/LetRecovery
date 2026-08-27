use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::tr;
use lr_core::boot_pca::BootPcaMode;
use lr_core::unattend_account::BuiltInAdministratorOptions;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

fn read_optional_bounded_plain_file(
    path: &Path,
    maximum_bytes: u64,
) -> std::io::Result<Option<Vec<u8>>> {
    match lr_core::scoped_temp_file::read_bounded_plain_file_pinned(path, maximum_bytes) {
        Ok((bytes, pins)) => {
            pins.verify_unchanged()?;
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Exact files changed while preparing a PE backup handoff.
///
/// The caller can restore only this transaction if the later BCD step fails, without deleting
/// unrelated PE resources or an older valid configuration.
pub struct BackupConfigTransaction {
    marker_path: PathBuf,
    marker_previous: Option<Vec<u8>>,
    marker_pins: lr_core::scoped_temp_file::PinnedDirectoryAncestors,
    boot_config_bytes: Option<Vec<u8>>,
    boot_manifest_bytes: Option<Vec<u8>>,
}

/// Exact files changed while preparing a PE install handoff.
///
/// The marker is published last. Until that atomic replace succeeds, PE cannot discover the new
/// task. A later BCD failure can restore every file touched by this session byte-for-byte.
#[must_use = "the PE install handoff must be explicitly committed or rolled back"]
pub struct InstallConfigTransaction {
    marker_path: PathBuf,
    marker_previous: Option<Vec<u8>>,
    custom_unattend: Option<(PathBuf, Option<Vec<u8>>)>,
    auto_partition_marker: Option<(PathBuf, Vec<u8>)>,
    target_marker: (PathBuf, Option<Vec<u8>>),
    full_disk_markers: Vec<(PathBuf, Option<Vec<u8>>)>,
    data_dir: PathBuf,
    session_id: String,
    data_dir_created: bool,
    active: bool,
    boot_config_bytes: Option<Vec<u8>>,
    boot_manifest_bytes: Option<Vec<u8>>,
    private_wifi_profile: Option<Vec<u8>>,
    protected_administrator_secret: Option<zeroize::Zeroizing<Vec<u8>>>,
}

/// Exact files changed while preparing a PE expansion handoff.
///
/// A failed later PE/BCD step can restore an older marker and INI byte-for-byte, or remove only
/// files created by this transaction. Unrelated files in the data directory are never removed.
pub struct ExpandConfigTransaction {
    marker_path: PathBuf,
    marker_previous: Option<Vec<u8>>,
    boot_config_bytes: Option<Vec<u8>>,
    boot_manifest_bytes: Option<Vec<u8>>,
    session_id: String,
}

impl ExpandConfigTransaction {
    pub(crate) fn take_boot_config_bytes(&mut self) -> Result<Vec<u8>> {
        self.boot_config_bytes
            .take()
            .context("expand authenticated boot config was already consumed")
    }

    pub(crate) fn take_boot_manifest_bytes(&mut self) -> Result<Vec<u8>> {
        self.boot_manifest_bytes
            .take()
            .context("expand authenticated boot manifest was already consumed")
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn rollback(self) -> Result<()> {
        restore_file(&self.marker_path, self.marker_previous.as_deref())
            .context(tr!("回滚扩容标记文件失败"))
    }
}

impl BackupConfigTransaction {
    pub(crate) fn take_boot_config_bytes(&mut self) -> Result<Vec<u8>> {
        self.boot_config_bytes
            .take()
            .context("backup authenticated boot config was already consumed")
    }
    pub(crate) fn take_boot_manifest_bytes(&mut self) -> Result<Vec<u8>> {
        self.boot_manifest_bytes
            .take()
            .context("backup authenticated boot manifest was already consumed")
    }
    pub fn rollback(self) -> Result<()> {
        self.marker_pins
            .verify_unchanged()
            .context("backup marker directory changed before rollback")?;
        restore_file(&self.marker_path, self.marker_previous.as_deref())
            .context(tr!("回滚备份标记文件失败"))?;
        self.marker_pins
            .verify_unchanged()
            .context("backup marker directory changed during rollback")?;
        Ok(())
    }
}

impl InstallConfigTransaction {
    pub(crate) fn take_boot_config_bytes(&mut self) -> Result<Vec<u8>> {
        self.boot_config_bytes
            .take()
            .context("install authenticated boot config was already consumed")
    }
    pub(crate) fn take_boot_manifest_bytes(&mut self) -> Result<Vec<u8>> {
        self.boot_manifest_bytes
            .take()
            .context("install authenticated boot manifest was already consumed")
    }
    pub(crate) fn take_private_wifi_profile(&mut self) -> Option<Vec<u8>> {
        self.private_wifi_profile.take()
    }
    pub(crate) fn take_protected_administrator_secret(
        &mut self,
    ) -> Option<zeroize::Zeroizing<Vec<u8>>> {
        self.protected_administrator_secret.take()
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn data_directory(&self) -> &Path {
        &self.data_dir
    }

    fn rollback_inner(&self) -> Result<()> {
        // The marker is the publication record. Remove the new record before changing any of the
        // data it names, then publish the previous marker only after all previous data is back.
        remove_file_checked(&self.marker_path).context("unpublish PE install marker")?;

        let mut failures = Vec::new();
        if let Some((path, previous)) = &self.custom_unattend {
            if let Err(error) = restore_file(path, previous.as_deref()) {
                failures.push(format!("custom unattend: {error}"));
            }
        }
        if let Some((path, previous)) = &self.auto_partition_marker {
            if let Err(error) = restore_file(path, Some(previous)) {
                failures.push(format!("automatic partition marker: {error}"));
            }
        }
        if let Err(error) = restore_file(&self.target_marker.0, self.target_marker.1.as_deref()) {
            failures.push(format!("install target marker: {error}"));
        }
        for (path, previous) in self.full_disk_markers.iter().rev() {
            if let Err(error) = restore_file(path, previous.as_deref()) {
                failures.push(format!("full-disk locator {}: {error}", path.display()));
            }
        }
        if self.data_dir_created {
            let _ = std::fs::remove_dir(&self.data_dir);
        }

        if !failures.is_empty() {
            anyhow::bail!(failures.join("; "));
        }

        restore_file(&self.marker_path, self.marker_previous.as_deref())
            .context("restore previous PE install marker")?;
        verify_file_state(&self.marker_path, self.marker_previous.as_deref())
            .context("verify previous PE install marker")
    }

    pub fn commit(mut self) {
        self.active = false;
    }

    pub fn rollback(mut self) -> Result<()> {
        let result = self.rollback_inner();
        self.active = false;
        result
    }
}

impl Drop for InstallConfigTransaction {
    fn drop(&mut self) {
        if self.active {
            if let Err(error) = self.rollback_inner() {
                log::error!("failed to roll back dropped PE install handoff: {error}");
            }
            self.active = false;
        }
    }
}

fn remove_file_checked(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if path.exists() {
        return Err(std::io::Error::other(format!(
            "file remains after removal: {}",
            path.display()
        )));
    }
    Ok(())
}

fn verify_file_state(path: &Path, expected: Option<&[u8]>) -> std::io::Result<()> {
    match expected {
        Some(expected) => {
            let actual = std::fs::read(path)?;
            if actual != expected {
                return Err(std::io::Error::other(format!(
                    "file read-back differs: {}",
                    path.display()
                )));
            }
            Ok(())
        }
        None => {
            if path.exists() {
                Err(std::io::Error::other(format!(
                    "file unexpectedly exists: {}",
                    path.display()
                )))
            } else {
                Ok(())
            }
        }
    }
}

fn restore_file(path: &Path, previous: Option<&[u8]>) -> std::io::Result<()> {
    let result = if let Some(previous) = previous {
        write_atomic_file(path, previous)
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    };
    result?;
    verify_file_state(path, previous)
}

fn write_atomic_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        )
    })?;
    let temporary = lr_core::scoped_temp_file::ScopedTempFile::create_in(
        parent,
        "lr-backup-config",
        "tmp",
        contents,
    )?;
    atomic_replace(temporary.path(), path)?;
    verify_file_state(path, Some(contents))
}

fn write_system_administrators_atomic_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(test)]
    {
        write_atomic_file(path, contents)
    }
    #[cfg(not(test))]
    {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no parent directory",
            )
        })?;
        let (temporary, mut writer) =
            lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
                parent,
                "lr-install-target",
                "tmp",
            )?;
        std::io::Write::write_all(&mut writer, contents)?;
        writer.sync_all()?;
        lr_core::scoped_temp_file::verify_system_administrators_file_custody(&writer)?;
        drop(writer);
        temporary.persist_replace(path)?;
        let published = std::fs::File::open(path)?;
        lr_core::scoped_temp_file::verify_system_administrators_file_custody(&published)?;
        verify_file_state(path, Some(contents))
    }
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| std::io::Error::other(format!("atomic replace failed: {error}")))
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

/// 系统安装配置（用于PE环境内安装）
#[derive(Debug, Clone, Default)]
pub struct InstallConfig {
    /// 本次安装任务会话ID，用于 PE 端绑定 marker 与配置。
    pub session_id: String,
    /// 无人值守安装
    pub unattended: bool,
    /// 驱动还原（兼容旧版本）
    pub restore_drivers: bool,
    /// 驱动操作模式: 0=无, 1=仅保存, 2=自动导入
    pub driver_action_mode: u8,
    /// 立即重启
    pub auto_reboot: bool,
    /// Authenticated disposable-VM automation policy. It is never populated by GUI preferences.
    pub automation_shutdown_on_terminal: bool,
    /// 在 PE 释放镜像前格式化目标分区。
    pub format_partition: bool,
    /// 在 PE 中保留六类本地个人目录并删除旧系统树，不格式化目标卷。
    pub preserve_personal_files: bool,
    /// 在 PE 释放镜像后写入或修复目标系统引导。
    pub repair_boot: bool,
    /// 原系统引导GUID（用于删除旧引导项）
    pub original_guid: String,
    /// 安装分卷索引
    pub volume_index: u32,
    /// 目标分区盘符
    pub target_partition: String,
    /// Authenticated shared install topology plan. It is serialized only into the private boot
    /// payload; public volumes contain random locator markers, never a second plan copy.
    pub custom_install_plan: lr_core::custom_install::CustomInstallPlan,
    /// Normal-endpoint-only topology identity used solely when authorizing removal of an automatic
    /// staging partition in the same provider-reclaimed tail. It is never serialized as an installation-target selector for
    /// WinPE; ordinary target discovery uses only the independent random target marker.
    pub canonical_target: Option<lr_core::install_handoff::CanonicalInstallTargetV2>,
    /// 镜像文件路径（相对于数据分区）
    pub image_path: String,
    /// 是否为GHO格式
    pub is_gho: bool,
    /// A private boot-WIM Wi-Fi profile is requested. Only its authenticated length/hash are
    /// serialized; the profile XML itself is never written to the public handoff volume.
    pub migrate_wifi: bool,
    pub wifi_profile_length: u64,
    pub wifi_profile_sha256: String,

    // 高级选项
    /// 移除快捷方式小箭头
    pub remove_shortcut_arrow: bool,
    /// Win11恢复经典右键
    pub restore_classic_context_menu: bool,
    /// OOBE绕过强制联网
    pub bypass_nro: bool,
    /// Remove the audited active Windows Update component surface on Windows 11 build 26100.
    /// The serialized legacy field name is retained for configuration compatibility.
    pub disable_windows_update: bool,
    /// Windows Security UI is distinct from the preserved Security Health/Firewall services.
    /// Remove the Defender Antivirus engine and exactly target the Windows Security UI AppX;
    /// SecurityHealthService, wscsvc, mpssvc, and firewall services remain preserved.
    pub disable_windows_defender: bool,
    /// 禁用系统保留空间
    pub disable_reserved_storage: bool,
    /// 禁用用户账户控制
    pub disable_uac: bool,
    /// 禁用自动设备加密
    pub disable_device_encryption: bool,
    /// 仅精确删除共享清单中的预配 AppX；旧配置的 true 值也使用这一收窄语义。
    /// Exact curated offline AppX cleanup while preserving Outlook and both AppX/Win32 OneDrive.
    /// uninstaller hook when the target is confirmed Win10/11 and built-in unattend is used.
    pub remove_uwp_apps: bool,
    /// 导入磁盘控制器驱动
    pub import_storage_controller_drivers: bool,
    /// 自定义用户名
    pub custom_username: String,
    /// 内置 RID-500 Administrator 的无人值守配置；密码只写入短期 PE 交接文件。
    pub builtin_administrator: BuiltInAdministratorOptions,
    /// 自定义系统盘卷标
    pub volume_label: String,
    /// 自定义无人值守文件：UI 选择时为源文件绝对路径；
    /// 经 write_install_config 复制到数据目录后，写入 INI 的是相对文件名。
    pub custom_unattend_path: String,
    /// URL-safe base64 JSON containing the exact preinstalled-software selection. The value is
    /// authenticated together with this configuration; installers themselves are bound by the
    /// public-data manifest.
    pub preinstalled_software_config: String,

    // Win7 专用选项
    /// Win7 UEFI 补丁（使用 UefiSeven）
    pub win7_uefi_patch: bool,
    /// Win7 注入USB3驱动
    pub win7_inject_usb3_driver: bool,
    /// Win7 注入NVMe驱动
    pub win7_inject_nvme_driver: bool,
    /// Win7 修复ACPI蓝屏
    pub win7_fix_acpi_bsod: bool,
    /// Win7 修复存储控制器蓝屏
    pub win7_fix_storage_bsod: bool,

    /// WIM 镜像引擎：0=libwim（默认），1=wimgapi。随重启传给 PE 端，使其使用相同引擎。
    pub wim_engine: u8,

    /// 目标镜像是否为 XP/2003（NT 5.x）。为真时 PE 端写 XP 引导（ntldr/boot.ini 或 UEFI/GPT）而非 bcdboot。
    pub is_xp: bool,

    /// Original I386/AMD64 text-mode media staged as a directory for PE execution.
    pub is_xp_i386: bool,
    /// Safe single directory component beneath the staged source root (`I386` or `AMD64`).
    pub xp_source_arch: String,

    // XP 专用选项（仅 is_xp 为真时生效；AHCI 始终注入，无开关）
    /// XP 注入 USB3(xHCI) 驱动（检测到 XP 时默认勾选）
    pub xp_inject_usb3_driver: bool,
    /// XP 注入 NVMe 驱动（检测到 XP 时默认勾选）
    pub xp_inject_nvme_driver: bool,

    /// 历史只读兼容字段；新配置固定为 false，旧脚本不得执行。
    pub run_diskpart_scripts: bool,
    /// 引导模式：0=自动，1=UEFI，2=Legacy。
    pub boot_mode: u8,
    /// UEFI Windows Boot Manager 签名选择。
    pub boot_pca_mode: BootPcaMode,
    /// PCA2023 兼容包在数据目录中的安全相对路径；空表示不需要。
    pub pca_compat_package: String,
    /// 暂存兼容包的 SHA-256。
    pub pca_compat_sha256: String,
    /// 兼容包内要提取的 WIM 卷索引。
    pub pca_compat_image_index: u32,
    /// 兼容包绑定的目标 Windows build。
    pub pca_compat_target_build: u32,
    /// 兼容包绑定的目标 WIM architecture 值。
    pub pca_compat_target_architecture: u16,
}

impl InstallConfig {
    /// 根据DriverAction获取driver_action_mode值
    pub fn driver_action_to_mode(action: crate::core::ui_state::DriverAction) -> u8 {
        match action {
            crate::core::ui_state::DriverAction::None => 0,
            crate::core::ui_state::DriverAction::SaveOnly => 1,
            crate::core::ui_state::DriverAction::AutoImport => 2,
        }
    }

    /// 判断是否需要导入驱动
    pub fn should_import_drivers(&self) -> bool {
        // 优先使用新的driver_action_mode
        if self.driver_action_mode > 0 {
            self.driver_action_mode == 2 // AutoImport
        } else {
            // 兼容旧版本
            self.restore_drivers
        }
    }
}

/// 系统备份配置（用于PE环境内备份）
#[derive(Debug, Clone, Default)]
pub struct BackupConfig {
    /// 备份保存路径（相对路径）
    pub save_path: String,
    /// 备份名称
    pub name: String,
    /// 备份描述
    pub description: String,
    /// 源分区盘符
    pub source_partition: String,
    /// 是否增量备份
    pub incremental: bool,
    /// 备份格式: 0=WIM, 1=ESD, 2=SWM, 3=GHO
    pub format: u8,
    /// SWM分卷大小（MB）
    pub swm_split_size: u32,
    /// WIM 镜像引擎：0=libwim（默认），1=wimgapi。随重启传给 PE 端。
    pub wim_engine: u8,
    /// Versioned source/destination/session authorization. Required for every new PE handoff.
    pub handoff: Option<lr_core::backup_handoff::BackupHandoffV2>,
}

/// 无损扩容配置：进 PE 后无损扩大目标分区（通常为当前系统盘 C:）。
#[derive(Debug, Clone, Default)]
pub struct ExpandConfig {
    pub session_id: String,
    /// 要扩大的目标分区（如 "C:"）。
    pub target_partition: String,
    /// 期望的最终总大小（MB）；0 表示尽可能扩到最大。
    pub target_size_mb: u64,
    /// WIM 引擎选择（随重启传给 PE，保持与其它流程一致）：0=libwim，1=wimgapi。
    pub wim_engine: u8,
    /// 从目标分区左侧的相邻数据分区让出空间，并把目标分区整体左移后扩展。
    pub borrow_from_left: bool,
    /// 相邻分区转移后供体分区的精确最终大小；0 保持旧扩容流程的按需收缩语义。
    pub donor_target_size_mb: u64,
    /// PE 写盘前必须重新匹配的物理磁盘与目标/供体几何；零表示旧配置未提供。
    pub expected_disk_number: u32,
    pub expected_disk_size_bytes: u64,
    pub expected_partition_number: u32,
    pub expected_partition_offset_bytes: u64,
    pub expected_partition_size_bytes: u64,
    pub expected_donor_partition_number: u32,
    pub expected_donor_offset_bytes: u64,
    pub expected_donor_size_bytes: u64,
}

/// 配置文件管理器
pub struct ConfigFileManager;

impl ConfigFileManager {
    /// 临时数据目录名
    const DATA_DIR: &'static str = "LetRecovery_Data";

    fn validate_ini_value(field: &str, value: &str) -> Result<()> {
        if value
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
        {
            anyhow::bail!("{field} contains a line break or NUL character");
        }
        Ok(())
    }

    fn validate_install_ini_values(config: &InstallConfig) -> Result<()> {
        for (field, value) in [
            ("SessionId", config.session_id.as_str()),
            ("OriginalGUID", config.original_guid.as_str()),
            ("TargetPartition", config.target_partition.as_str()),
            ("ImagePath", config.image_path.as_str()),
            ("XpSourceArch", config.xp_source_arch.as_str()),
            ("PcaCompatPackage", config.pca_compat_package.as_str()),
            ("PcaCompatSha256", config.pca_compat_sha256.as_str()),
            ("CustomUsername", config.custom_username.as_str()),
            (
                "BuiltinAdministratorName",
                config.builtin_administrator.account_name.as_str(),
            ),
            (
                "BuiltinAdministratorPassword",
                config.builtin_administrator.password.expose_secret(),
            ),
            ("VolumeLabel", config.volume_label.as_str()),
            ("CustomUnattendFile", config.custom_unattend_path.as_str()),
            (
                "PreinstalledSoftwareConfig",
                config.preinstalled_software_config.as_str(),
            ),
        ] {
            Self::validate_ini_value(field, value)?;
        }
        Ok(())
    }

    /// 自动创建分区的标志文件名（与 disk.rs 中的常量保持一致）
    const AUTO_CREATED_PARTITION_MARKER: &'static str = "LetRecovery_AutoCreated.marker";

    pub(crate) fn new_session_id() -> Result<String> {
        Ok(lr_core::handoff_auth::generate_session_id()?
            .as_str()
            .to_owned())
    }

    pub(crate) fn public_artifact_record(
        data_partition: &str,
        identity: &lr_core::install_source_lock::LockedSourceArtifactIdentity,
        role: lr_core::handoff_manifest::ArtifactRole,
        ordinal: u32,
    ) -> Result<lr_core::handoff_manifest::ArtifactRecord> {
        let root = std::fs::canonicalize(PathBuf::from(format!(
            "{}\\",
            data_partition.trim_end_matches(['\\', '/'])
        )))
        .context("canonicalize public handoff data root")?;
        let relative = identity.path.strip_prefix(&root).with_context(|| {
            format!(
                "handoff artifact is outside the authorized public data volume: {}",
                identity.path.display()
            )
        })?;
        let relative_path = relative
            .to_str()
            .context("handoff artifact relative path is not Unicode")?
            .replace('/', "\\");
        Ok(lr_core::handoff_manifest::ArtifactRecord {
            role,
            location: lr_core::handoff_manifest::ArtifactLocation::PublicData,
            ordinal,
            relative_path,
            length_bytes: identity.length_bytes,
            sha256: identity.sha256,
        })
    }

    pub fn write_install_config_transactional(
        target_partition: &str,
        data_partition: &str,
        config: &InstallConfig,
        _auth_key: &lr_core::handoff_auth::SessionAuthKey,
        source_artifacts: Vec<lr_core::handoff_manifest::ArtifactRecord>,
    ) -> Result<InstallConfigTransaction> {
        Self::write_install_config_transactional_with_private_wifi(
            target_partition,
            data_partition,
            config,
            _auth_key,
            source_artifacts,
            None,
            None,
        )
    }

    pub(crate) fn write_install_config_transactional_with_private_wifi(
        target_partition: &str,
        data_partition: &str,
        config: &InstallConfig,
        _auth_key: &lr_core::handoff_auth::SessionAuthKey,
        mut source_artifacts: Vec<lr_core::handoff_manifest::ArtifactRecord>,
        private_wifi_profile: Option<&[u8]>,
        auto_staging_source_length_before_bytes: Option<u64>,
    ) -> Result<InstallConfigTransaction> {
        let mut config = config.clone();
        if config.session_id.trim().is_empty() {
            config.session_id = Self::new_session_id()?;
        }
        lr_core::handoff_auth::validate_session_id(&config.session_id)?;
        let private_wifi_profile = private_wifi_profile
            .map(|bytes| {
                let binding = lr_core::first_logon::PrivateWifiProfileBinding::from_bytes(bytes)?;
                config.migrate_wifi = true;
                config.wifi_profile_length = binding.length_bytes;
                config.wifi_profile_sha256 = binding.sha256;
                Ok::<_, anyhow::Error>(bytes.to_vec())
            })
            .transpose()?;
        if private_wifi_profile.is_none() {
            config.migrate_wifi = false;
            config.wifi_profile_length = 0;
            config.wifi_profile_sha256.clear();
        }
        if !config.custom_unattend_path.is_empty() {
            anyhow::bail!(
                "ViaPE custom unattend requires a protected boot artifact and is temporarily fail-closed"
            );
        }
        let protected_administrator_secret = if config.builtin_administrator.enabled {
            config
                .builtin_administrator
                .validate()
                .context("validate built-in Administrator handoff")?;
            let secret = lr_core::unattend_account::serialize_protected_administrator_secret(
                &config.builtin_administrator.password,
            )
            .map_err(anyhow::Error::msg)?;
            let sha256 = lr_core::install_handoff::decode_hex_array::<32>(
                &lr_core::hash::sha256_bytes(&secret),
                "protected Administrator secret SHA-256",
            )?;
            source_artifacts.push(lr_core::handoff_manifest::ArtifactRecord {
                role: lr_core::handoff_manifest::ArtifactRole::ProtectedAdministratorSecret,
                location: lr_core::handoff_manifest::ArtifactLocation::ProtectedBoot,
                ordinal: 0,
                relative_path: lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_FILE_NAME
                    .to_owned(),
                length_bytes: secret.len() as u64,
                sha256,
            });
            Some(secret)
        } else {
            None
        };
        // Password material is never serialized into the public data-volume INI. PE reconstructs
        // it only from the manifest-bound file held inside this session's private boot WIM.
        config.builtin_administrator.password.clear();
        Self::validate_ini_value("target_partition", target_partition)?;
        Self::validate_ini_value("data_partition", data_partition)?;
        Self::validate_install_ini_values(&config)?;

        let data_locator_token = lr_core::handoff_auth::generate_locator_token()?;
        let target_locator_token = lr_core::handoff_auth::generate_locator_token()?;
        let target_marker_path = PathBuf::from(format!(
            "{}\\{}",
            target_partition.trim_end_matches(['\\', '/']),
            lr_core::install_handoff::INSTALL_TARGET_MARKER_NAME
        ));
        let target_marker_previous = read_optional_bounded_plain_file(&target_marker_path, 4096)
            .context("read previous installation target marker")?;
        let target_marker_bytes =
            lr_core::install_handoff::locator_marker_bytes(target_locator_token.as_str())?;
        let full_disk_marker_specs = match &config.custom_install_plan {
            lr_core::custom_install::CustomInstallPlan::RepartitionAllDisks(plan) => {
                crate::core::custom_install_plan::full_disk_locator_paths(plan)?
            }
            _ => Vec::new(),
        };
        let mut full_disk_markers = Vec::with_capacity(full_disk_marker_specs.len());
        for (path, _) in &full_disk_marker_specs {
            let previous = read_optional_bounded_plain_file(path, 4096)
                .with_context(|| format!("read previous full-disk locator {}", path.display()))?;
            full_disk_markers.push((path.clone(), previous));
        }

        let data_dir = PathBuf::from(format!("{}\\{}", data_partition, Self::DATA_DIR));
        let data_dir_created = !data_dir.exists();
        std::fs::create_dir_all(&data_dir).context(tr!("创建数据目录失败"))?;
        // The private boot WIM authenticates both independent random locator values.
        let marker_path = PathBuf::from(format!(
            "{}\\{}",
            data_partition,
            lr_core::install_handoff::DATA_VOLUME_MARKER_NAME
        ));
        let marker_previous = marker_path
            .is_file()
            .then(|| std::fs::read(&marker_path))
            .transpose()?;
        let custom_path = data_dir.join("custom_unattend.xml");
        let custom_previous = (!config.custom_unattend_path.is_empty() && custom_path.is_file())
            .then(|| std::fs::read(&custom_path))
            .transpose()?;
        let auto_marker_path = PathBuf::from(format!(
            "{}\\{}",
            data_partition,
            Self::AUTO_CREATED_PARTITION_MARKER
        ));
        let auto_marker_previous = auto_marker_path
            .is_file()
            .then(|| std::fs::read(&auto_marker_path))
            .transpose()?;
        let mut transaction = InstallConfigTransaction {
            marker_path: marker_path.clone(),
            marker_previous,
            custom_unattend: (!config.custom_unattend_path.is_empty())
                .then(|| (custom_path.clone(), custom_previous)),
            auto_partition_marker: auto_marker_previous
                .clone()
                .map(|previous| (auto_marker_path.clone(), previous)),
            target_marker: (target_marker_path.clone(), target_marker_previous),
            full_disk_markers,
            data_dir: data_dir.clone(),
            session_id: config.session_id.clone(),
            data_dir_created,
            active: true,
            boot_config_bytes: None,
            boot_manifest_bytes: None,
            private_wifi_profile,
            protected_administrator_secret,
        };
        if let Err(error) = write_atomic_file(&target_marker_path, &target_marker_bytes) {
            if let Err(rollback) = transaction.rollback() {
                return Err(anyhow::anyhow!(
                    "{error}; additionally failed to roll back installation target marker: {rollback}"
                ));
            }
            return Err(error).context("publish installation target marker");
        }
        for (path, token) in &full_disk_marker_specs {
            let bytes = lr_core::install_handoff::locator_marker_bytes(token)?;
            if let Err(error) = write_atomic_file(path, &bytes) {
                if let Err(rollback) = transaction.rollback() {
                    return Err(anyhow::anyhow!(
                        "{error}; additionally failed to roll back full-disk locators: {rollback}"
                    ));
                }
                return Err(error)
                    .with_context(|| format!("publish full-disk locator {}", path.display()));
            }
        }
        let mut manifest_auto_staging = None;
        let mut locked_auto_marker = None;

        // An automatically-created staging partition is deleted in PE after the install. Bind
        // that irreversible cleanup authorization to this exact handoff, target extent, staging
        // extent, and canonical layout token. The old human-readable marker alone is not an
        // authorization record and must never be accepted by PE for deletion.
        if auto_marker_previous.is_some() {
            let source_length_before_bytes =
                auto_staging_source_length_before_bytes.ok_or_else(|| {
                    anyhow::anyhow!(
                        "automatic staging transaction is missing its pre-shrink source extent"
                    )
                })?;
            let target_letter = lr_core::windows_storage::path_drive_letter(Path::new(&format!(
                "{}\\",
                target_partition.trim_end_matches(['\\', '/'])
            )))
            .ok_or_else(|| anyhow::anyhow!("cannot resolve automatic-partition source drive"))?;
            let data_letter = lr_core::windows_storage::path_drive_letter(Path::new(&format!(
                "{}\\",
                data_partition.trim_end_matches(['\\', '/'])
            )))
            .ok_or_else(|| anyhow::anyhow!("cannot resolve automatic staging drive"))?;
            let target = lr_core::windows_storage::stable_volume_identity(target_letter)
                .context("capture automatic-partition source identity")?;
            let staging = lr_core::windows_storage::stable_volume_identity(data_letter)
                .context("capture automatic staging identity")?;
            let layout = lr_core::windows_storage::disk_layout_snapshot(target.extent.disk_number)
                .context("capture canonical automatic-staging disk layout")?;
            if config.canonical_target.is_none() {
                config.canonical_target = Some(
                    lr_core::install_handoff::CanonicalInstallTargetV2::from_snapshot(
                        &layout,
                        target.extent.offset_bytes,
                        target.extent.extent_length_bytes,
                    )
                    .context("build canonical automatic-staging source extent")?,
                );
            }
            let canonical = config
                .canonical_target
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("canonical target missing after capture"))?;
            let source_end = target
                .extent
                .offset_bytes
                .checked_add(target.extent.extent_length_bytes);
            if target.extent.disk_number != staging.extent.disk_number
                || target.extent.offset_bytes != canonical.partition_offset_bytes
                || target.extent.extent_length_bytes != canonical.partition_length_bytes
                || source_end.is_none_or(|end| end > staging.extent.offset_bytes)
            {
                return transaction
                    .rollback()
                    .and_then(|()| anyhow::bail!("automatic staging partition is not a non-overlapping provider-created extent after the authorized installation source"));
            }
            let temporary = lr_core::install_handoff::CanonicalInstallTargetV2::from_snapshot(
                &layout,
                staging.extent.offset_bytes,
                staging.extent.extent_length_bytes,
            )
            .context("build canonical automatic-staging extent")?;
            manifest_auto_staging = Some(lr_core::handoff_manifest::AutoStagingAuthorization {
                source: canonical.clone(),
                temporary,
                source_length_before_bytes,
            });
            let bound_marker = format!(
                "LetRecovery Auto Created Partition\r\nMarkerVersion=3\r\nSessionId={}\r\nSource={}:\r\nSourceDisk={}\r\nSourceOffsetBytes={}\r\nSourceLengthBytes={}\r\nSourceLengthBeforeBytes={}\r\nTemporaryDisk={}\r\nTemporaryOffsetBytes={}\r\nTemporaryLengthBytes={}\r\nCanonicalDiskLayoutSha256={}\r\n",
                config.session_id,
                target_letter,
                target.extent.disk_number,
                target.extent.offset_bytes,
                target.extent.extent_length_bytes,
                source_length_before_bytes,
                staging.extent.disk_number,
                staging.extent.offset_bytes,
                staging.extent.extent_length_bytes,
                lr_core::install_handoff::encode_hex(&canonical.layout_digest),
            );
            if let Err(error) = write_atomic_file(&auto_marker_path, bound_marker.as_bytes()) {
                if let Err(rollback) = transaction.rollback() {
                    return Err(anyhow::anyhow!(
                        "{error}; additionally failed to roll back automatic partition authorization: {rollback}"
                    ));
                }
                return Err(error)
                    .context("bind automatic staging partition to installation session");
            }
            let marker_lock =
                lr_core::install_source_lock::LockedPlainArtifact::acquire(&auto_marker_path)
                    .map_err(anyhow::Error::msg)
                    .context("lock automatic partition marker for authenticated manifest")?;
            source_artifacts.push(Self::public_artifact_record(
                data_partition,
                marker_lock.identity(),
                lr_core::handoff_manifest::ArtifactRole::AutoPartitionMarker,
                0,
            )?);
            locked_auto_marker = Some(marker_lock);
        }

        // 处理自定义无人值守文件：把用户选择的 XML 复制到数据目录，INI 里只存相对文件名
        if !config.custom_unattend_path.is_empty() {
            const CUSTOM_UNATTEND_NAME: &str = "custom_unattend.xml";
            let contents = match std::fs::read(&config.custom_unattend_path) {
                Ok(contents) => contents,
                Err(error) => {
                    if let Err(rollback) = transaction.rollback() {
                        return Err(anyhow::anyhow!(
                            "{error}; additionally failed to roll back handoff: {rollback}"
                        ));
                    }
                    return Err(error).with_context(|| {
                        tr!(
                            "读取自定义无人值守文件失败: {}",
                            config.custom_unattend_path
                        )
                    });
                }
            };
            if let Err(error) = write_atomic_file(&custom_path, &contents) {
                if let Err(rollback) = transaction.rollback() {
                    return Err(anyhow::anyhow!(
                        "{error}; additionally failed to roll back handoff: {rollback}"
                    ));
                }
                return Err(error).context(tr!("写入自定义无人值守文件失败"));
            }
            config.custom_unattend_path = CUSTOM_UNATTEND_NAME.to_string();
            log::info!(
                "[CONFIG] 已原子写入自定义无人值守文件 -> {}",
                custom_path.display()
            );
        }

        // Keep the historical configuration key readable, but new sessions never stage or
        // execute arbitrary partition scripts after the storage path moved to typed WinAPI.
        config.run_diskpart_scripts = false;

        let manifest = lr_core::handoff_manifest::HandoffManifest::new(
            lr_core::handoff_auth::HandoffPurpose::Install,
            config.session_id.clone(),
            data_locator_token.as_str(),
            Some(target_locator_token.as_str().to_owned()),
            manifest_auto_staging,
            source_artifacts,
        )?
        .to_bytes()?;
        let manifest_binding = lr_core::handoff_manifest::ManifestBinding::new(&manifest)?;
        let mut content = Self::serialize_install_config(&config)?;
        content.push_str("[HandoffManifest]\r\n");
        content.push_str(&manifest_binding.to_config_lines());
        lr_core::install_handoff::validate_install_handoff_ini(&content)
            .context("validate manifest-bound installation handoff")?;
        transaction.boot_config_bytes = Some(content.as_bytes().to_vec());
        transaction.boot_manifest_bytes = Some(manifest);
        if let Err(error) = write_atomic_file(
            &marker_path,
            &lr_core::install_handoff::locator_marker_bytes(data_locator_token.as_str())?,
        ) {
            if let Err(rollback) = transaction.rollback() {
                return Err(anyhow::anyhow!(
                    "{error}; additionally failed to roll back handoff: {rollback}"
                ));
            }
            return Err(error).context(tr!("写入安装标记文件失败"));
        }
        if let Some(marker) = &locked_auto_marker {
            marker
                .verify_unchanged()
                .map_err(anyhow::Error::msg)
                .context("automatic partition marker changed during handoff publication")?;
        }

        log::info!("[CONFIG] authoritative install config prepared in private boot payload");
        log::info!(
            "[CONFIG] install data locator published: {}",
            marker_path.display()
        );

        Ok(transaction)
    }

    pub fn write_expand_config_transactional(
        target_partition: &str,
        data_partition: &str,
        config: &ExpandConfig,
        _auth_key: &lr_core::handoff_auth::SessionAuthKey,
    ) -> Result<ExpandConfigTransaction> {
        let mut config = config.clone();
        if config.session_id.is_empty() {
            config.session_id = Self::new_session_id()?;
        }
        lr_core::handoff_auth::validate_session_id(&config.session_id)?;
        Self::validate_ini_value("target_partition", target_partition)?;
        Self::validate_ini_value("data_partition", data_partition)?;
        Self::validate_ini_value("TargetPartition", &config.target_partition)?;
        let data_locator_token = lr_core::handoff_auth::generate_locator_token()?;
        let marker_path = PathBuf::from(format!(
            "{}\\{}",
            data_partition,
            lr_core::install_handoff::DATA_VOLUME_MARKER_NAME
        ));
        let marker_previous = if marker_path.exists() {
            Some(std::fs::read(&marker_path).context(tr!("读取原扩容标记文件失败"))?)
        } else {
            None
        };
        let mut transaction = ExpandConfigTransaction {
            marker_path: marker_path.clone(),
            marker_previous,
            boot_config_bytes: None,
            boot_manifest_bytes: None,
            session_id: config.session_id.clone(),
        };

        let manifest = lr_core::handoff_manifest::HandoffManifest::new(
            lr_core::handoff_auth::HandoffPurpose::Expand,
            config.session_id.clone(),
            data_locator_token.as_str(),
            None,
            None,
            Vec::new(),
        )?
        .to_bytes()?;
        let manifest_binding = lr_core::handoff_manifest::ManifestBinding::new(&manifest)?;
        let content = format!(
            "[Expand]\r\nSessionId={}\r\nTargetPartition={}\r\nTargetSizeMb={}\r\nWimEngine={}\r\nBorrowFromLeft={}\r\nDonorTargetSizeMb={}\r\nExpectedDiskNumber={}\r\nExpectedDiskSizeBytes={}\r\nExpectedPartitionNumber={}\r\nExpectedPartitionOffsetBytes={}\r\nExpectedPartitionSizeBytes={}\r\nExpectedDonorPartitionNumber={}\r\nExpectedDonorOffsetBytes={}\r\nExpectedDonorSizeBytes={}\r\nLanguage={}\r\n{}",
            config.session_id,
            config.target_partition,
            config.target_size_mb,
            config.wim_engine,
            config.borrow_from_left,
            config.donor_target_size_mb,
            config.expected_disk_number,
            config.expected_disk_size_bytes,
            config.expected_partition_number,
            config.expected_partition_offset_bytes,
            config.expected_partition_size_bytes,
            config.expected_donor_partition_number,
            config.expected_donor_offset_bytes,
            config.expected_donor_size_bytes,
            crate::utils::i18n::current_language(),
            manifest_binding.to_config_lines()
        );
        transaction.boot_config_bytes = Some(content.as_bytes().to_vec());
        transaction.boot_manifest_bytes = Some(manifest);
        if let Err(error) = write_atomic_file(
            &marker_path,
            &lr_core::install_handoff::locator_marker_bytes(data_locator_token.as_str())?,
        ) {
            if let Err(rollback) = transaction.rollback() {
                return Err(anyhow::anyhow!(
                    "{error}; additionally failed to roll back handoff: {rollback}"
                ));
            }
            return Err(error).context(tr!("写入扩容标记文件失败"));
        }

        log::info!("[CONFIG] authoritative expand config prepared in private boot payload");
        log::info!(
            "[CONFIG] expand data locator published: {}",
            marker_path.display()
        );
        Ok(transaction)
    }

    pub fn write_backup_config_transactional(
        source_partition: &str,
        data_partition: &str,
        config: &BackupConfig,
        _auth_key: &lr_core::handoff_auth::SessionAuthKey,
    ) -> Result<BackupConfigTransaction> {
        Self::validate_ini_value("source_partition", source_partition)?;
        Self::validate_ini_value("data_partition", data_partition)?;
        for (field, value) in [
            ("SavePath", config.save_path.as_str()),
            ("Name", config.name.as_str()),
            ("Description", config.description.as_str()),
            ("SourcePartition", config.source_partition.as_str()),
        ] {
            Self::validate_ini_value(field, value)?;
        }
        // Build and validate the complete LRBK2 payload before creating or replacing any
        // filesystem object. A missing/invalid authorization must be side-effect free.
        let handoff = config
            .handoff
            .as_ref()
            .context("backup handoff has no LRBK2 authorization")?;
        lr_core::handoff_auth::validate_session_id(&handoff.session_id)?;
        let data_locator_token = lr_core::handoff_auth::generate_locator_token()?;
        let manifest = lr_core::handoff_manifest::HandoffManifest::new(
            lr_core::handoff_auth::HandoffPurpose::Backup,
            handoff.session_id.clone(),
            data_locator_token.as_str(),
            None,
            None,
            Vec::new(),
        )?
        .to_bytes()?;
        let manifest_binding = lr_core::handoff_manifest::ManifestBinding::new(&manifest)?;
        let mut content = Self::serialize_backup_config(config)?;
        content.push_str(&manifest_binding.to_config_lines());
        if content.len() > 128 * 1024 {
            anyhow::bail!("backup configuration exceeds the 128 KiB handoff limit");
        }
        let (_, parsed_handoff) = lr_core::backup_handoff::parse_backup_payload(&content)
            .context("validate the complete LRBK2 payload before publication")?;
        if &parsed_handoff != handoff {
            anyhow::bail!("serialized LRBK2 authorization changed during validation");
        }
        lr_core::handoff_auth::validate_session_id(&handoff.session_id)?;
        let data_root = PathBuf::from(format!(
            "{}\\",
            data_partition.trim_end_matches(['\\', '/'])
        ));
        let data_root_pins =
            lr_core::scoped_temp_file::pin_existing_directory_ancestors(&data_root)
                .context("pin backup data volume ancestors")?;
        data_root_pins.verify_unchanged()?;

        // The destination data volume is rediscovered only by this WIM-authenticated token.
        let marker_path = data_root.join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME);
        let marker_parent = marker_path
            .parent()
            .context("backup marker has no volume root")?;
        let marker_pins =
            lr_core::scoped_temp_file::pin_existing_directory_ancestors(marker_parent)
                .context("pin backup marker volume ancestors")?;
        marker_pins.verify_unchanged()?;
        let marker_previous = read_optional_bounded_plain_file(&marker_path, 4 * 1024)
            .context(tr!("读取原备份标记文件失败"))?;
        let transaction = BackupConfigTransaction {
            marker_path: marker_path.clone(),
            marker_previous,
            marker_pins,
            boot_config_bytes: Some(content.as_bytes().to_vec()),
            boot_manifest_bytes: Some(manifest),
        };

        transaction.marker_pins.verify_unchanged()?;
        if let Err(error) = write_atomic_file(
            &marker_path,
            &lr_core::install_handoff::locator_marker_bytes(data_locator_token.as_str())?,
        ) {
            if let Err(rollback) = transaction.rollback() {
                return Err(anyhow::anyhow!(
                    "{error}; additionally failed to roll back handoff: {rollback}"
                ));
            }
            return Err(error).context(tr!("写入备份标记文件失败"));
        }
        transaction.marker_pins.verify_unchanged()?;

        log::info!("[CONFIG] authoritative backup config prepared in private boot payload");
        log::info!(
            "[CONFIG] backup data locator published: {}",
            marker_path.display()
        );
        Ok(transaction)
    }

    /// 获取数据目录路径
    pub fn get_data_dir(partition: &str) -> String {
        format!("{}\\{}", partition, Self::DATA_DIR)
    }

    /// 序列化安装配置为INI格式
    fn serialize_install_config(config: &InstallConfig) -> Result<String> {
        // Cross-reboot installation discovery is token-only. Canonical disk inventory is kept in
        // normal-endpoint planning objects where it is useful, but is not serialized into the PE
        // install task and can never become a second target-selection gate.
        let canonical_target = String::new();
        let custom_install_plan = config
            .custom_install_plan
            .to_json()
            .context("serialize authenticated custom installation plan")?;
        let wifi_binding = if config.migrate_wifi {
            let sha256 = config.wifi_profile_sha256.trim().to_ascii_lowercase();
            if config.wifi_profile_length == 0
                || config.wifi_profile_length > lr_core::first_logon::PRIVATE_WIFI_PROFILE_MAX_BYTES
                || sha256.len() != 64
                || !sha256
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                anyhow::bail!("private Wi-Fi profile binding is invalid");
            }
            format!(
                "MigrateWifi=true\r\nWifiProfileLength={}\r\nWifiProfileSha256={}\r\n",
                config.wifi_profile_length, sha256
            )
        } else {
            String::new()
        };
        Ok(format!(
            r#"[Install]
SessionId={}
Unattended={}
RestoreDrivers={}
DriverActionMode={}
AutoReboot={}
AutomationShutdownOnTerminal={}
FormatPartition={}
PreservePersonalFiles={}
RepairBoot={}
OriginalGUID={}
VolumeIndex={}
TargetPartition={}
CustomInstallPlanJson={}
ImagePath={}
IsGho={}
WimEngine={}
IsXp={}
IsXpI386={}
XpSourceArch={}
RunDiskpartScripts={}
BootMode={}
BootPcaMode={}
PcaCompatPackage={}
PcaCompatSha256={}
PcaCompatImageIndex={}
PcaCompatTargetBuild={}
PcaCompatTargetArchitecture={}
Language={}
{}{}

[Advanced]
RemoveShortcutArrow={}
RestoreClassicContextMenu={}
BypassNRO={}
DisableWindowsUpdate={}
DisableWindowsDefender={}
DisableReservedStorage={}
DisableUAC={}
DisableDeviceEncryption={}
RemoveUWPApps={}
ImportStorageControllerDrivers={}
CustomUsername={}
BuiltinAdministrator={}
BuiltinAdministratorName={}
BuiltinAdministratorPassword={}
BuiltinAdministratorAutoLogon={}
VolumeLabel={}
CustomUnattendFile={}
PreinstalledSoftwareConfig={}

[Win7]
Win7UefiPatch={}
Win7InjectUsb3Driver={}
Win7InjectNvmeDriver={}
Win7FixAcpiBsod={}
Win7FixStorageBsod={}

[Xp]
XpInjectUsb3Driver={}
XpInjectNvmeDriver={}
"#,
            config.session_id,
            config.unattended,
            config.restore_drivers,
            config.driver_action_mode,
            config.auto_reboot,
            config.automation_shutdown_on_terminal,
            config.format_partition,
            config.preserve_personal_files,
            config.repair_boot,
            config.original_guid,
            config.volume_index,
            config.target_partition,
            custom_install_plan,
            config.image_path,
            config.is_gho,
            config.wim_engine,
            config.is_xp,
            config.is_xp_i386,
            config.xp_source_arch,
            false,
            config.boot_mode,
            config.boot_pca_mode.as_config_value(),
            config.pca_compat_package,
            config.pca_compat_sha256,
            config.pca_compat_image_index,
            config.pca_compat_target_build,
            config.pca_compat_target_architecture,
            crate::utils::i18n::current_language(),
            wifi_binding,
            canonical_target,
            config.remove_shortcut_arrow,
            config.restore_classic_context_menu,
            config.bypass_nro,
            config.disable_windows_update,
            config.disable_windows_defender,
            config.disable_reserved_storage,
            config.disable_uac,
            config.disable_device_encryption,
            config.remove_uwp_apps,
            config.import_storage_controller_drivers,
            config.custom_username,
            config.builtin_administrator.enabled,
            config.builtin_administrator.account_name,
            config.builtin_administrator.password.expose_secret(),
            config.builtin_administrator.auto_logon,
            config.volume_label,
            config.custom_unattend_path,
            config.preinstalled_software_config,
            config.win7_uefi_patch,
            config.win7_inject_usb3_driver,
            config.win7_inject_nvme_driver,
            config.win7_fix_acpi_bsod,
            config.win7_fix_storage_bsod,
            config.xp_inject_usb3_driver,
            config.xp_inject_nvme_driver,
        ))
    }

    /// 序列化备份配置为INI格式
    fn serialize_backup_config(config: &BackupConfig) -> Result<String> {
        let handoff = config
            .handoff
            .as_ref()
            .context("backup configuration has no LRBK2 authorization")?;
        let handoff_fields = handoff.serialize_fields()?;
        Ok(format!(
            r#"[Backup]
SavePath={}
Name={}
Description={}
SourcePartition={}
Incremental={}
Format={}
SwmSplitSize={}
WimEngine={}
Language={}
{}
"#,
            config.save_path,
            config.name,
            config.description,
            config.source_partition,
            config.incremental,
            config.format,
            config.swm_split_size,
            config.wim_engine,
            crate::utils::i18n::current_language(),
            handoff_fields,
        ))
    }

    /// 反序列化安装配置
    fn deserialize_install_config(content: &str) -> Result<InstallConfig> {
        lr_core::install_handoff::validate_install_handoff_ini(content)
            .context("validate installation handoff syntax")?;
        // Older handoff files predate these switches and always performed both
        // operations. Keep that behavior unless the normal endpoint explicitly
        // persists a newer value.
        let mut config = InstallConfig {
            format_partition: true,
            repair_boot: true,
            ..InstallConfig::default()
        };
        let mut canonical_version = None;
        let mut canonical_digest = None::<String>;
        let mut canonical_offset = None;
        let mut canonical_length = None;
        let mut canonical_style = None::<String>;
        let mut canonical_gpt_id = None::<String>;
        let mut canonical_storage_id = None::<String>;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "SessionId" => config.session_id = value.to_string(),
                    "Unattended" => config.unattended = value.parse().unwrap_or(false),
                    "RestoreDrivers" => config.restore_drivers = value.parse().unwrap_or(false),
                    "DriverActionMode" => config.driver_action_mode = value.parse().unwrap_or(0),
                    "AutoReboot" => config.auto_reboot = value.parse().unwrap_or(false),
                    "AutomationShutdownOnTerminal" => {
                        config.automation_shutdown_on_terminal =
                            value.parse().with_context(|| {
                                format!("invalid AutomationShutdownOnTerminal boolean: {value}")
                            })?
                    }
                    "FormatPartition" => {
                        config.format_partition = value
                            .parse::<bool>()
                            .with_context(|| format!("invalid FormatPartition boolean: {value}"))?
                    }
                    "PreservePersonalFiles" => {
                        config.preserve_personal_files =
                            value.parse::<bool>().with_context(|| {
                                format!("invalid PreservePersonalFiles boolean: {value}")
                            })?
                    }
                    "RepairBoot" => {
                        config.repair_boot = value
                            .parse::<bool>()
                            .with_context(|| format!("invalid RepairBoot boolean: {value}"))?
                    }
                    "OriginalGUID" => config.original_guid = value.to_string(),
                    "VolumeIndex" => config.volume_index = value.parse().unwrap_or(1),
                    "TargetPartition" => config.target_partition = value.to_string(),
                    "CustomInstallPlanJson" => {
                        config.custom_install_plan =
                            lr_core::custom_install::CustomInstallPlan::from_json(value)
                                .context("parse authenticated custom installation plan")?
                    }
                    "CanonicalTargetVersion" => canonical_version = Some(value.parse()?),
                    "CanonicalDiskLayoutSha256" => canonical_digest = Some(value.to_string()),
                    "CanonicalPartitionOffsetBytes" => canonical_offset = Some(value.parse()?),
                    "CanonicalPartitionLengthBytes" => canonical_length = Some(value.parse()?),
                    "CanonicalDiskStyle" => canonical_style = Some(value.to_string()),
                    "CanonicalGptPartitionId" => canonical_gpt_id = Some(value.to_string()),
                    "CanonicalStorageIdSha256" => canonical_storage_id = Some(value.to_string()),
                    "ImagePath" => config.image_path = value.to_string(),
                    "IsGho" => config.is_gho = value.parse().unwrap_or(false),
                    "WimEngine" => config.wim_engine = value.parse().unwrap_or(0),
                    "IsXp" => config.is_xp = value.parse().unwrap_or(false),
                    "IsXpI386" => config.is_xp_i386 = value.parse().unwrap_or(false),
                    "XpSourceArch" => config.xp_source_arch = value.to_string(),
                    "RunDiskpartScripts" => {
                        config.run_diskpart_scripts = value.parse().unwrap_or(false)
                    }
                    "BootMode" => config.boot_mode = value.parse().unwrap_or(0),
                    "BootPcaMode" => config.boot_pca_mode = BootPcaMode::from_config_value(value),
                    "PcaCompatPackage" => config.pca_compat_package = value.to_string(),
                    "PcaCompatSha256" => config.pca_compat_sha256 = value.to_string(),
                    "PcaCompatImageIndex" => {
                        config.pca_compat_image_index = value.parse().unwrap_or(0)
                    }
                    "PcaCompatTargetBuild" => {
                        config.pca_compat_target_build = value.parse().unwrap_or(0)
                    }
                    "PcaCompatTargetArchitecture" => {
                        config.pca_compat_target_architecture = value.parse().unwrap_or(0)
                    }
                    "MigrateWifi" => config.migrate_wifi = value.parse().unwrap_or(false),
                    "WifiProfileLength" => config.wifi_profile_length = value.parse().unwrap_or(0),
                    "WifiProfileSha256" => config.wifi_profile_sha256 = value.to_string(),
                    "RemoveShortcutArrow" => {
                        config.remove_shortcut_arrow = value.parse().unwrap_or(false)
                    }
                    "RestoreClassicContextMenu" => {
                        config.restore_classic_context_menu = value.parse().unwrap_or(false)
                    }
                    "BypassNRO" => config.bypass_nro = value.parse().unwrap_or(false),
                    "DisableWindowsUpdate" => {
                        config.disable_windows_update = value.parse().unwrap_or(false)
                    }
                    "DisableWindowsDefender" => {
                        config.disable_windows_defender = value.parse().unwrap_or(false)
                    }
                    "DisableReservedStorage" => {
                        config.disable_reserved_storage = value.parse().unwrap_or(false)
                    }
                    "DisableUAC" => config.disable_uac = value.parse().unwrap_or(false),
                    "DisableDeviceEncryption" => {
                        config.disable_device_encryption = value.parse().unwrap_or(false)
                    }
                    "RemoveUWPApps" => config.remove_uwp_apps = value.parse().unwrap_or(false),
                    "ImportStorageControllerDrivers" => {
                        config.import_storage_controller_drivers = value.parse().unwrap_or(false)
                    }
                    "CustomUsername" => config.custom_username = value.to_string(),
                    "BuiltinAdministrator" => {
                        config.builtin_administrator.enabled = value.parse().unwrap_or(false)
                    }
                    "BuiltinAdministratorName" => {
                        config.builtin_administrator.account_name = value.to_string()
                    }
                    "BuiltinAdministratorPassword" => {
                        config.builtin_administrator.password = value.into()
                    }
                    "BuiltinAdministratorAutoLogon" => {
                        config.builtin_administrator.auto_logon = value.parse().unwrap_or(false)
                    }
                    "VolumeLabel" => config.volume_label = value.to_string(),
                    "CustomUnattendFile" => config.custom_unattend_path = value.to_string(),
                    "PreinstalledSoftwareConfig" => {
                        config.preinstalled_software_config = value.to_string()
                    }
                    "Win7UefiPatch" => config.win7_uefi_patch = value.parse().unwrap_or(false),
                    "Win7InjectUsb3Driver" => {
                        config.win7_inject_usb3_driver = value.parse().unwrap_or(false)
                    }
                    "Win7InjectNvmeDriver" => {
                        config.win7_inject_nvme_driver = value.parse().unwrap_or(false)
                    }
                    "Win7FixAcpiBsod" => config.win7_fix_acpi_bsod = value.parse().unwrap_or(false),
                    "Win7FixStorageBsod" => config.win7_fix_storage_bsod = false,
                    "XpInjectUsb3Driver" => {
                        config.xp_inject_usb3_driver = value.parse().unwrap_or(false)
                    }
                    "XpInjectNvmeDriver" => {
                        config.xp_inject_nvme_driver = value.parse().unwrap_or(false)
                    }
                    _ => {}
                }
            }
        }

        config.canonical_target = lr_core::install_handoff::canonical_target_from_fields(
            canonical_version,
            canonical_digest.as_deref(),
            canonical_offset,
            canonical_length,
            canonical_style.as_deref(),
            canonical_gpt_id.as_deref(),
            canonical_storage_id.as_deref(),
        )?;
        let expected_wifi = lr_core::first_logon::private_wifi_binding_from_install_ini(content)?;
        match expected_wifi {
            Some(binding) => {
                if !config.migrate_wifi
                    || config.wifi_profile_length != binding.length_bytes
                    || config.wifi_profile_sha256 != binding.sha256
                {
                    anyhow::bail!("Wi-Fi binding fields were not parsed consistently");
                }
            }
            None => {
                config.migrate_wifi = false;
                config.wifi_profile_length = 0;
                config.wifi_profile_sha256.clear();
            }
        }
        if config.preserve_personal_files {
            if config.format_partition {
                anyhow::bail!("personal-file preservation conflicts with target formatting");
            }
            if config.is_gho || config.is_xp || config.is_xp_i386 {
                anyhow::bail!(
                    "personal-file preservation requires a Windows 7+ WIM/ESD/SWM source"
                );
            }
            if config.custom_install_plan.mode()
                != lr_core::custom_install::CustomInstallMode::ReinstallPartition
            {
                anyhow::bail!("personal-file preservation only supports partition reinstall");
            }
        }
        Ok(config)
    }

    /// 反序列化备份配置
    fn deserialize_backup_config(content: &str) -> Result<BackupConfig> {
        let (values, handoff) = lr_core::backup_handoff::parse_backup_payload(content)?;
        Ok(BackupConfig {
            save_path: values.save_path,
            name: values.name,
            description: values.description,
            source_partition: values.source_partition,
            incremental: values.incremental,
            format: values.format,
            swm_split_size: values.swm_split_size,
            wim_engine: values.wim_engine,
            handoff: Some(handoff),
        })
    }
}

/// unattend.xml 语法校验，基于 roxmltree 做完整 XML 解析。
///
/// 相比手写扫描器，roxmltree 能完整检查标签配对、嵌套、属性引号、实体、
/// 命名空间等，并在出错时给出行列号，便于用户定位。
/// 返回 Ok(()) 表示语法合法；Err(msg) 给出可展示给用户的错误原因。
pub fn validate_unattend_xml(xml: &str) -> Result<(), String> {
    let s = xml.trim_start_matches('\u{feff}');
    if s.trim().is_empty() {
        return Err(tr!("文件内容为空"));
    }

    // 完整 XML 解析：标签未闭合/未配对、引号未闭合、非法嵌套等都会在此报错。
    let doc = roxmltree::Document::parse(s).map_err(|e| tr!("XML 语法错误：{}", e))?;

    // 根元素必须是 <unattend>
    let root = doc.root_element();
    let root_name = root.tag_name().name();
    if root_name != "unattend" {
        return Err(tr!(
            "不是有效的无人值守文件（根元素应为 <unattend>，实际为 <{}>）",
            if root_name.is_empty() { "?" } else { root_name }
        ));
    }

    Ok(())
}

/// XP/2003 的 winnt.sif 应答轻校验。
///
/// winnt.sif 是 INI 风格(不是 XML),只做基本健全性检查:非空、且至少含一个
/// XP 应答常见节(`[Unattended]` / `[Data]` / `[GuiUnattended]` / `[UserData]`)。
/// 返回 Ok(()) 表示看起来是有效的 winnt.sif；Err(msg) 给出可展示的原因。
pub fn validate_winnt_sif(content: &str) -> Result<(), String> {
    let s = content.trim_start_matches('\u{feff}');
    if s.trim().is_empty() {
        return Err(tr!("文件内容为空"));
    }
    let lower = s.to_ascii_lowercase();
    let has_section = ["[unattended]", "[data]", "[guiunattended]", "[userdata]"]
        .iter()
        .any(|sec| lower.contains(sec));
    if !has_section {
        return Err(
            tr!("不像有效的 winnt.sif(缺少 [Unattended]/[Data]/[GuiUnattended] 等节)。XP/2003 应答文件为 INI 格式的 winnt.sif,不是 XML。")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_test_target() -> lr_core::install_handoff::CanonicalInstallTargetV2 {
        lr_core::install_handoff::CanonicalInstallTargetV2 {
            layout_digest: [0xab; 32],
            device_id_hash: Some([0xcd; 32]),
            partition_offset_bytes: 1_048_576,
            partition_length_bytes: 8_589_934_592,
            style: lr_core::install_handoff::CanonicalTargetStyle::Mbr,
            gpt_partition_id: None,
        }
    }

    fn backup_test_handoff() -> lr_core::backup_handoff::BackupHandoffV2 {
        let source = canonical_test_target();
        let mut destination = canonical_test_target();
        destination.layout_digest = [0x31; 32];
        destination.device_id_hash = Some([0x42; 32]);
        destination.partition_offset_bytes = 2_097_152;
        lr_core::backup_handoff::BackupHandoffV2 {
            session_id: "0123456789abcdef0123456789abcdef".to_owned(),
            source,
            destination,
            destination_relative_path: PathBuf::from("backup.esd"),
            output_policy: lr_core::backup_handoff::BackupOutputPolicy::Append,
            base_file: Some(lr_core::backup_handoff::BackupBaseFileIdentity {
                length_bytes: 4096,
                sha256: [0x55; 32],
            }),
        }
    }

    fn handoff_test_key() -> lr_core::handoff_auth::SessionAuthKey {
        lr_core::handoff_auth::SessionAuthKey::from_bytes([0x5a; 32]).unwrap()
    }

    fn unique_temp_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "letrecovery-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn old_install_config_defaults_to_auto_boot_selection() {
        let config = ConfigFileManager::deserialize_install_config(
            "[Install]\r\nVolumeIndex=3\r\nTargetPartition=C:\r\n",
        )
        .unwrap();

        assert_eq!(config.volume_index, 3);
        assert!(config.format_partition);
        assert!(config.repair_boot);
        assert_eq!(config.boot_mode, 0);
        assert_eq!(config.boot_pca_mode, BootPcaMode::Auto);
        assert!(!config.is_xp_i386);
        assert!(config.xp_source_arch.is_empty());
    }

    #[test]
    fn explicit_invalid_destructive_switches_fail_closed() {
        for content in [
            "[Install]\r\nFormatPartition=fasle\r\n",
            "[Install]\r\nRepairBoot=garbage\r\n",
        ] {
            assert!(ConfigFileManager::deserialize_install_config(content).is_err());
        }
    }

    #[test]
    fn win7_uefi_and_processor_workarounds_are_preserved_but_storage_hack_is_ignored() {
        let config = ConfigFileManager::deserialize_install_config(concat!(
            "[Install]\r\n",
            "Win7UefiPatch=true\r\n",
            "Win7FixAcpiBsod=true\r\n",
            "Win7FixStorageBsod=true\r\n"
        ))
        .unwrap();

        assert!(config.win7_uefi_patch);
        assert!(config.win7_fix_acpi_bsod);
        assert!(!config.win7_fix_storage_bsod);
    }

    #[test]
    fn install_config_round_trips_boot_selection() {
        let source = InstallConfig {
            volume_index: 1,
            canonical_target: Some(canonical_test_target()),
            format_partition: false,
            repair_boot: false,
            automation_shutdown_on_terminal: true,
            boot_mode: 1,
            boot_pca_mode: BootPcaMode::Pca2023,
            pca_compat_package: "pca_compat\\package.wim".to_string(),
            pca_compat_sha256: "a".repeat(64),
            pca_compat_image_index: 1,
            pca_compat_target_build: 19045,
            pca_compat_target_architecture: 9,
            is_xp_i386: true,
            xp_source_arch: "I386".to_string(),
            ..InstallConfig::default()
        };

        let serialized = ConfigFileManager::serialize_install_config(&source).unwrap();
        let parsed = ConfigFileManager::deserialize_install_config(&serialized).unwrap();

        assert!(!parsed.format_partition);
        assert!(!parsed.repair_boot);
        assert!(parsed.automation_shutdown_on_terminal);
        assert!(serialized.contains("AutomationShutdownOnTerminal=true"));
        assert_eq!(parsed.boot_mode, 1);
        assert_eq!(parsed.boot_pca_mode, BootPcaMode::Pca2023);
        assert_eq!(parsed.pca_compat_package, "pca_compat\\package.wim");
        assert_eq!(parsed.pca_compat_sha256, "a".repeat(64));
        assert_eq!(parsed.pca_compat_image_index, 1);
        assert_eq!(parsed.pca_compat_target_build, 19045);
        assert_eq!(parsed.pca_compat_target_architecture, 9);
        assert!(parsed.is_xp_i386);
        assert_eq!(parsed.xp_source_arch, "I386");
        assert_eq!(parsed.canonical_target, None);
    }

    #[test]
    fn install_config_round_trips_personal_file_preservation_without_formatting() {
        let source = InstallConfig {
            volume_index: 1,
            canonical_target: Some(canonical_test_target()),
            format_partition: false,
            preserve_personal_files: true,
            custom_install_plan: lr_core::custom_install::CustomInstallPlan::ReinstallPartition,
            ..InstallConfig::default()
        };

        let serialized = ConfigFileManager::serialize_install_config(&source).unwrap();
        let parsed = ConfigFileManager::deserialize_install_config(&serialized).unwrap();

        assert!(serialized.contains("PreservePersonalFiles=true"));
        assert!(parsed.preserve_personal_files);
        assert!(!parsed.format_partition);
    }

    #[test]
    fn install_config_round_trips_only_the_private_wifi_binding() {
        let profile = b"<WLANProfile><name>test</name></WLANProfile>";
        let binding = lr_core::first_logon::PrivateWifiProfileBinding::from_bytes(profile).unwrap();
        let source = InstallConfig {
            volume_index: 1,
            migrate_wifi: true,
            wifi_profile_length: binding.length_bytes,
            wifi_profile_sha256: binding.sha256.clone(),
            ..InstallConfig::default()
        };

        let serialized = ConfigFileManager::serialize_install_config(&source).unwrap();
        assert!(!serialized.contains("<WLANProfile>"));
        let parsed = ConfigFileManager::deserialize_install_config(&serialized).unwrap();
        assert!(parsed.migrate_wifi);
        assert_eq!(parsed.wifi_profile_length, binding.length_bytes);
        assert_eq!(parsed.wifi_profile_sha256, binding.sha256);
    }

    #[test]
    fn install_config_round_trips_authenticated_preinstalled_software_selection() {
        let packages = [lr_core::software_install::SelectedSoftwarePackage {
            id: "vmware-tools-x64".to_owned(),
            name: "VMware Tools".to_owned(),
            download_url: "https://packages.vmware.com/tools/tool.exe".to_owned(),
            filename: "VMware-tools.exe".to_owned(),
            silent_command: r#""{installer}" /S /v"/qn REBOOT=R""#.to_owned(),
            requires_admin: true,
        }];
        let encoded = lr_core::software_install::encode_selected_packages(&packages).unwrap();
        let source = InstallConfig {
            volume_index: 1,
            preinstalled_software_config: encoded.clone(),
            ..InstallConfig::default()
        };

        let serialized = ConfigFileManager::serialize_install_config(&source).unwrap();
        assert!(serialized.contains("PreinstalledSoftwareConfig="));
        assert!(!serialized.contains("packages.vmware.com"));
        let parsed = ConfigFileManager::deserialize_install_config(&serialized).unwrap();
        assert_eq!(parsed.preinstalled_software_config, encoded);
        assert_eq!(
            lr_core::software_install::decode_selected_packages(
                &parsed.preinstalled_software_config
            )
            .unwrap(),
            packages
        );
    }

    #[test]
    fn low_level_install_ini_serializer_round_trips_builtin_administrator_credentials() {
        let source = InstallConfig {
            volume_index: 1,
            builtin_administrator: BuiltInAdministratorOptions {
                enabled: true,
                account_name: "LocalAdmin".to_string(),
                password: "temporary-secret".into(),
                auto_logon: true,
            },
            ..InstallConfig::default()
        };

        let serialized = ConfigFileManager::serialize_install_config(&source).unwrap();
        let parsed = ConfigFileManager::deserialize_install_config(&serialized).unwrap();

        assert!(parsed.builtin_administrator.enabled);
        assert_eq!(parsed.builtin_administrator.account_name, "LocalAdmin");
        assert_eq!(
            parsed.builtin_administrator.password.expose_secret(),
            "temporary-secret"
        );
        assert!(parsed.builtin_administrator.auto_logon);
    }

    #[test]
    fn via_pe_transaction_moves_administrator_password_out_of_the_config_ini() {
        let root = unique_temp_root("protected-administrator");
        let target = root.join("target");
        let data = root.join("data");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let config = InstallConfig {
            volume_index: 1,
            target_partition: target.to_string_lossy().into_owned(),
            image_path: "install.wim".into(),
            builtin_administrator: BuiltInAdministratorOptions {
                enabled: true,
                account_name: "LocalAdmin".to_owned(),
                password: "temporary-secret".into(),
                auto_logon: true,
            },
            ..InstallConfig::default()
        };
        let source_artifacts = vec![lr_core::handoff_manifest::ArtifactRecord {
            role: lr_core::handoff_manifest::ArtifactRole::InstallImageSpan,
            location: lr_core::handoff_manifest::ArtifactLocation::PublicData,
            ordinal: 0,
            relative_path: "LetRecovery_Data\\install.wim".into(),
            length_bytes: 1,
            sha256: [1; 32],
        }];

        let mut transaction = ConfigFileManager::write_install_config_transactional(
            &target.to_string_lossy(),
            &data.to_string_lossy(),
            &config,
            &handoff_test_key(),
            source_artifacts,
        )
        .unwrap();
        let config_bytes = transaction.take_boot_config_bytes().unwrap();
        let config_text = std::str::from_utf8(&config_bytes).unwrap();
        assert!(config_text.contains("BuiltinAdministrator=true"));
        assert!(config_text.contains("BuiltinAdministratorName=LocalAdmin"));
        assert!(config_text
            .lines()
            .any(|line| line == "BuiltinAdministratorPassword="));
        assert!(!config_text.contains("temporary-secret"));

        let manifest = lr_core::handoff_manifest::HandoffManifest::parse(
            &transaction.take_boot_manifest_bytes().unwrap(),
        )
        .unwrap();
        let record = manifest
            .artifacts
            .iter()
            .find(|record| {
                record.role == lr_core::handoff_manifest::ArtifactRole::ProtectedAdministratorSecret
            })
            .unwrap();
        let secret = transaction.take_protected_administrator_secret().unwrap();
        assert_eq!(
            record.location,
            lr_core::handoff_manifest::ArtifactLocation::ProtectedBoot
        );
        assert_eq!(record.length_bytes, secret.len() as u64);
        assert_eq!(
            lr_core::unattend_account::parse_protected_administrator_secret(&secret)
                .unwrap()
                .as_str(),
            "temporary-secret"
        );
        assert_eq!(
            record.sha256,
            lr_core::install_handoff::decode_hex_array::<32>(
                &lr_core::hash::sha256_bytes(&secret),
                "test secret SHA-256",
            )
            .unwrap()
        );

        drop(transaction);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_handoff_transaction_restores_previous_task_byte_for_byte() {
        let root = unique_temp_root("install-transaction");
        let target = root.join("target");
        let data = root.join("data");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let data_dir = PathBuf::from(format!(
            "{}\\{}",
            data.display(),
            ConfigFileManager::DATA_DIR
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = PathBuf::from(format!(
            "{}\\{}",
            data.display(),
            lr_core::install_handoff::DATA_VOLUME_MARKER_NAME
        ));
        let ini = data_dir.join("LetRecovery_Install.ini");
        let custom = data_dir.join("custom_unattend.xml");
        std::fs::write(&marker, b"old marker").unwrap();
        std::fs::write(&ini, b"old config").unwrap();
        std::fs::write(&custom, b"old unattend").unwrap();
        let new_custom = root.join("new.xml");
        std::fs::write(&new_custom, b"new unattend").unwrap();
        let config = InstallConfig {
            custom_unattend_path: new_custom.to_string_lossy().into_owned(),
            target_partition: target.to_string_lossy().into_owned(),
            image_path: "install.wim".into(),
            canonical_target: Some(canonical_test_target()),
            ..InstallConfig::default()
        };

        let error = ConfigFileManager::write_install_config_transactional(
            &target.to_string_lossy(),
            &data.to_string_lossy(),
            &config,
            &handoff_test_key(),
            Vec::new(),
        )
        .err()
        .expect("custom unattend must fail closed until private boot payload support exists");
        assert!(error.to_string().contains("custom unattend"));
        assert_eq!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_eq!(std::fs::read(&ini).unwrap(), b"old config");
        assert_eq!(std::fs::read(&custom).unwrap(), b"old unattend");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dropped_install_handoff_transaction_rolls_back_automatically() {
        let root = unique_temp_root("install-drop-rollback");
        let target = root.join("target");
        let data = root.join("data");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let data_dir = PathBuf::from(format!(
            "{}\\{}",
            data.display(),
            ConfigFileManager::DATA_DIR
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = PathBuf::from(format!(
            "{}\\{}",
            data.display(),
            lr_core::install_handoff::DATA_VOLUME_MARKER_NAME
        ));
        let ini = data_dir.join("LetRecovery_Install.ini");
        let target_marker = target.join(lr_core::install_handoff::INSTALL_TARGET_MARKER_NAME);
        std::fs::write(&marker, b"old marker").unwrap();
        std::fs::write(&ini, b"old config").unwrap();
        std::fs::write(&target_marker, b"unrelated old target marker").unwrap();
        let config = InstallConfig {
            volume_index: 1,
            target_partition: target.to_string_lossy().into_owned(),
            image_path: "install.wim".into(),
            canonical_target: Some(canonical_test_target()),
            ..InstallConfig::default()
        };
        let source_artifacts = vec![lr_core::handoff_manifest::ArtifactRecord {
            role: lr_core::handoff_manifest::ArtifactRole::InstallImageSpan,
            location: lr_core::handoff_manifest::ArtifactLocation::PublicData,
            ordinal: 0,
            relative_path: "LetRecovery_Data\\install.wim".into(),
            length_bytes: 1,
            sha256: [1; 32],
        }];

        {
            let mut transaction = ConfigFileManager::write_install_config_transactional(
                &target.to_string_lossy(),
                &data.to_string_lossy(),
                &config,
                &handoff_test_key(),
                source_artifacts,
            )
            .unwrap();
            assert_ne!(std::fs::read(&marker).unwrap(), b"old marker");
            assert_eq!(std::fs::read(&ini).unwrap(), b"old config");
            assert_ne!(
                std::fs::read(&target_marker).unwrap(),
                b"unrelated old target marker"
            );
            let manifest = lr_core::handoff_manifest::HandoffManifest::parse(
                &transaction.take_boot_manifest_bytes().unwrap(),
            )
            .unwrap();
            let target_token = manifest.install_target_token.as_deref().unwrap();
            assert_ne!(manifest.data_locator_token, target_token);
            assert_eq!(
                std::fs::read(&marker).unwrap(),
                manifest.data_locator_token.as_bytes()
            );
            assert_eq!(
                std::fs::read(&target_marker).unwrap(),
                target_token.as_bytes()
            );
            let boot_config = transaction.take_boot_config_bytes().unwrap();
            assert!(!String::from_utf8(boot_config)
                .unwrap()
                .contains("CanonicalDiskLayoutSha256"));
        }

        assert_eq!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_eq!(std::fs::read(&ini).unwrap(), b"old config");
        assert_eq!(
            std::fs::read(&target_marker).unwrap(),
            b"unrelated old target marker"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_transaction_restores_existing_files_and_preserves_unrelated_data() {
        let root = unique_temp_root("backup-restore");
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = root.join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME);
        let config_path = data_dir.join("LetRecovery_Backup.ini");
        let unrelated = data_dir.join("user-owned.txt");
        std::fs::write(&marker, b"old marker").unwrap();
        std::fs::write(&config_path, b"old config").unwrap();
        std::fs::write(&unrelated, b"keep me").unwrap();

        let partition = root.to_string_lossy();
        let transaction = ConfigFileManager::write_backup_config_transactional(
            &partition,
            &partition,
            &BackupConfig {
                save_path: "D:\\backup.wim".to_owned(),
                name: "System Backup".to_owned(),
                description: "Created by LetRecovery".to_owned(),
                source_partition: "C:".to_owned(),
                incremental: true,
                format: 0,
                swm_split_size: 4096,
                wim_engine: 0,
                handoff: Some(backup_test_handoff()),
            },
            &handoff_test_key(),
        )
        .unwrap();
        assert_ne!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_eq!(std::fs::read(&config_path).unwrap(), b"old config");

        transaction.rollback().unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_eq!(std::fs::read(&config_path).unwrap(), b"old config");
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep me");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_transaction_removes_only_files_created_by_this_write() {
        let root = unique_temp_root("backup-new");
        std::fs::create_dir_all(&root).unwrap();
        let partition = root.to_string_lossy();
        let transaction = ConfigFileManager::write_backup_config_transactional(
            &partition,
            &partition,
            &BackupConfig {
                save_path: "D:\\backup.esd".to_owned(),
                name: "System Backup".to_owned(),
                description: String::new(),
                source_partition: "C:".to_owned(),
                incremental: true,
                format: 1,
                swm_split_size: 4096,
                wim_engine: 1,
                handoff: Some(backup_test_handoff()),
            },
            &handoff_test_key(),
        )
        .unwrap();
        let marker = root.join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME);
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        let config_path = data_dir.join("LetRecovery_Backup.ini");
        assert!(marker.exists());
        assert!(!config_path.exists());

        transaction.rollback().unwrap();
        assert!(!marker.exists());
        assert!(!config_path.exists());
        assert!(!data_dir.exists());
        assert!(root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_marker_is_published_on_destination_and_never_on_capture_source() {
        let source = unique_temp_root("backup-marker-source");
        let destination = unique_temp_root("backup-marker-destination");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        let source_text = source.to_string_lossy();
        let destination_text = destination.to_string_lossy();
        let mut handoff = backup_test_handoff();
        handoff.output_policy = lr_core::backup_handoff::BackupOutputPolicy::Create;
        handoff.base_file = None;
        let transaction = ConfigFileManager::write_backup_config_transactional(
            &source_text,
            &destination_text,
            &BackupConfig {
                save_path: "D:\\backup.wim".to_owned(),
                name: "System Backup".to_owned(),
                description: String::new(),
                source_partition: "C:".to_owned(),
                incremental: false,
                format: 0,
                swm_split_size: 4096,
                wim_engine: 1,
                handoff: Some(handoff),
            },
            &handoff_test_key(),
        )
        .unwrap();

        assert!(!source
            .join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME)
            .exists());
        assert!(destination
            .join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME)
            .is_file());
        transaction.rollback().unwrap();
        assert!(!destination
            .join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME)
            .exists());
        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn expand_transaction_restores_existing_files_and_preserves_unrelated_data() {
        let root = unique_temp_root("expand-restore");
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = root.join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME);
        let config_path = data_dir.join("LetRecovery_Expand.ini");
        let unrelated = data_dir.join("user-owned.txt");
        std::fs::write(&marker, b"old marker").unwrap();
        std::fs::write(&config_path, b"old config").unwrap();
        std::fs::write(&unrelated, b"keep me").unwrap();

        let partition = root.to_string_lossy();
        let transaction = ConfigFileManager::write_expand_config_transactional(
            &partition,
            &partition,
            &ExpandConfig {
                session_id: String::new(),
                target_partition: "C:".to_owned(),
                target_size_mb: 123_456,
                wim_engine: 1,
                borrow_from_left: true,
                donor_target_size_mb: 123_000,
                expected_disk_number: 2,
                expected_disk_size_bytes: 1_000_000,
                expected_partition_number: 4,
                expected_partition_offset_bytes: 600_000,
                expected_partition_size_bytes: 200_000,
                expected_donor_partition_number: 3,
                expected_donor_offset_bytes: 200_000,
                expected_donor_size_bytes: 400_000,
            },
            &handoff_test_key(),
        )
        .unwrap();
        assert_ne!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_eq!(std::fs::read(&config_path).unwrap(), b"old config");

        transaction.rollback().unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_eq!(std::fs::read(&config_path).unwrap(), b"old config");
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep me");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_transaction_removes_only_files_created_by_this_write() {
        let root = unique_temp_root("expand-new");
        std::fs::create_dir_all(&root).unwrap();
        let partition = root.to_string_lossy();
        let transaction = ConfigFileManager::write_expand_config_transactional(
            &partition,
            &partition,
            &ExpandConfig {
                session_id: String::new(),
                target_partition: "C:".to_owned(),
                target_size_mb: 0,
                wim_engine: 0,
                borrow_from_left: false,
                donor_target_size_mb: 0,
                expected_disk_number: 0,
                expected_disk_size_bytes: 0,
                expected_partition_number: 0,
                expected_partition_offset_bytes: 0,
                expected_partition_size_bytes: 0,
                expected_donor_partition_number: 0,
                expected_donor_offset_bytes: 0,
                expected_donor_size_bytes: 0,
            },
            &handoff_test_key(),
        )
        .unwrap();
        let marker = root.join(lr_core::install_handoff::DATA_VOLUME_MARKER_NAME);
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        let config_path = data_dir.join("LetRecovery_Expand.ini");
        assert!(marker.exists());
        assert!(!config_path.exists());

        transaction.rollback().unwrap();
        assert!(!marker.exists());
        assert!(!config_path.exists());
        assert!(!data_dir.exists());
        assert!(root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
