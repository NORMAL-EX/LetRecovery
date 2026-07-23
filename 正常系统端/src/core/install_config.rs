use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::tr;
use lr_core::boot_pca::BootPcaMode;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

/// Exact files changed while preparing a PE backup handoff.
///
/// The caller can restore only this transaction if the later BCD step fails, without deleting
/// unrelated PE resources or an older valid configuration.
pub struct BackupConfigTransaction {
    marker_path: PathBuf,
    marker_previous: Option<Vec<u8>>,
    config_path: PathBuf,
    config_previous: Option<Vec<u8>>,
    data_dir: PathBuf,
    data_dir_created: bool,
}

/// Exact files changed while preparing a PE expansion handoff.
///
/// A failed later PE/BCD step can restore an older marker and INI byte-for-byte, or remove only
/// files created by this transaction. Unrelated files in the data directory are never removed.
pub struct ExpandConfigTransaction {
    marker_path: PathBuf,
    marker_previous: Option<Vec<u8>>,
    config_path: PathBuf,
    config_previous: Option<Vec<u8>>,
    data_dir: PathBuf,
    data_dir_created: bool,
}

impl ExpandConfigTransaction {
    pub fn rollback(self) -> Result<()> {
        let config_result = restore_file(&self.config_path, self.config_previous.as_deref())
            .context(tr!("回滚扩容配置文件失败"));
        let marker_result = restore_file(&self.marker_path, self.marker_previous.as_deref())
            .context(tr!("回滚扩容标记文件失败"));

        // `remove_dir` succeeds only when the directory is empty. If another file appeared after
        // this transaction began, preserving that file is more important than removing the folder.
        if self.data_dir_created {
            let _ = std::fs::remove_dir(&self.data_dir);
        }

        match (config_result, marker_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(config), Ok(())) => Err(config),
            (Ok(()), Err(marker)) => Err(marker),
            (Err(config), Err(marker)) => Err(anyhow::anyhow!("{config}; {marker}")),
        }
    }
}

impl BackupConfigTransaction {
    pub fn rollback(self) -> Result<()> {
        // Always attempt both restores. A failure restoring the INI must not leave the source
        // volume's marker pointing at a handoff that never committed, and vice versa.
        let config_result = restore_file(&self.config_path, self.config_previous.as_deref())
            .context(tr!("回滚备份配置文件失败"));
        let marker_result = restore_file(&self.marker_path, self.marker_previous.as_deref())
            .context(tr!("回滚备份标记文件失败"));
        if self.data_dir_created {
            let _ = std::fs::remove_dir(&self.data_dir);
        }

        match (config_result, marker_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(config), Ok(())) => Err(config),
            (Ok(()), Err(marker)) => Err(marker),
            (Err(config), Err(marker)) => Err(anyhow::anyhow!("{config}; {marker}")),
        }
    }
}

fn restore_file(path: &Path, previous: Option<&[u8]>) -> std::io::Result<()> {
    if let Some(previous) = previous {
        write_atomic_file(path, previous)
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
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
    atomic_replace(temporary.path(), path)
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

/// 递归复制目录（用于把 diskpart 脚本暂存到数据分区）。
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
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
    /// 原系统引导GUID（用于删除旧引导项）
    pub original_guid: String,
    /// 安装分卷索引
    pub volume_index: u32,
    /// 目标分区盘符
    pub target_partition: String,
    /// 镜像文件路径（相对于数据分区）
    pub image_path: String,
    /// 是否为GHO格式
    pub is_gho: bool,

    // 高级选项
    /// 移除快捷方式小箭头
    pub remove_shortcut_arrow: bool,
    /// Win11恢复经典右键
    pub restore_classic_context_menu: bool,
    /// OOBE绕过强制联网
    pub bypass_nro: bool,
    /// 禁用Windows自动更新
    pub disable_windows_update: bool,
    /// 深度移除 Microsoft Defender Antivirus 杀毒引擎（保留安全中心等组件）
    pub disable_windows_defender: bool,
    /// 禁用系统保留空间
    pub disable_reserved_storage: bool,
    /// 禁用用户账户控制
    pub disable_uac: bool,
    /// 禁用自动设备加密
    pub disable_device_encryption: bool,
    /// 删除预装UWP应用
    pub remove_uwp_apps: bool,
    /// 导入磁盘控制器驱动
    pub import_storage_controller_drivers: bool,
    /// 自定义用户名
    pub custom_username: String,
    /// 自定义系统盘卷标
    pub volume_label: String,
    /// 自定义无人值守文件：UI 选择时为源文件绝对路径；
    /// 经 write_install_config 复制到数据目录后，写入 INI 的是相对文件名。
    pub custom_unattend_path: String,

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

    /// 是否在释放镜像前运行 diskpart 脚本（程序目录\diskpart\ 下所有脚本）。
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

    /// 从driver_action_mode获取DriverAction
    pub fn mode_to_driver_action(mode: u8) -> crate::core::ui_state::DriverAction {
        match mode {
            0 => crate::core::ui_state::DriverAction::None,
            1 => crate::core::ui_state::DriverAction::SaveOnly,
            2 => crate::core::ui_state::DriverAction::AutoImport,
            // 兼容旧版本：如果restore_drivers为true则默认AutoImport
            _ => crate::core::ui_state::DriverAction::AutoImport,
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
}

/// 无损扩容配置：进 PE 后无损扩大目标分区（通常为当前系统盘 C:）。
#[derive(Debug, Clone, Default)]
pub struct ExpandConfig {
    /// 要扩大的目标分区（如 "C:"）。
    pub target_partition: String,
    /// 期望的最终总大小（MB）；0 表示尽可能扩到最大。
    pub target_size_mb: u64,
    /// WIM 引擎选择（随重启传给 PE，保持与其它流程一致）：0=libwim，1=wimgapi。
    pub wim_engine: u8,
}

/// 配置文件管理器
pub struct ConfigFileManager;

#[derive(Debug, Clone)]
struct InstallMarker {
    partition: String,
    session_id: String,
}

impl ConfigFileManager {
    /// 标记文件名
    const INSTALL_MARKER: &'static str = "LetRecovery_Install.marker";
    const BACKUP_MARKER: &'static str = "LetRecovery_Backup.marker";

    const EXPAND_MARKER: &'static str = "LetRecovery_Expand.marker";

    /// 配置文件名
    const INSTALL_CONFIG: &'static str = "LetRecovery_Install.ini";
    const BACKUP_CONFIG: &'static str = "LetRecovery_Backup.ini";
    const EXPAND_CONFIG: &'static str = "LetRecovery_Expand.ini";

    /// PE文件目录名
    const PE_DIR: &'static str = "LetRecovery_PE";

    /// 临时数据目录名
    const DATA_DIR: &'static str = "LetRecovery_Data";

    fn scan_letters() -> impl Iterator<Item = char> {
        (b'A'..=b'Z').map(char::from)
    }

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
            ("VolumeLabel", config.volume_label.as_str()),
            ("CustomUnattendFile", config.custom_unattend_path.as_str()),
        ] {
            Self::validate_ini_value(field, value)?;
        }
        Ok(())
    }

    /// 自动创建分区的标志文件名（与 disk.rs 中的常量保持一致）
    const AUTO_CREATED_PARTITION_MARKER: &'static str = "LetRecovery_AutoCreated.marker";

    fn new_session_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("LR{:x}{:x}", nanos, std::process::id())
    }

    fn normalize_partition(partition: &str) -> String {
        partition
            .chars()
            .find(|character| character.is_ascii_alphabetic())
            .map(|letter| format!("{}:", letter.to_ascii_uppercase()))
            .unwrap_or_default()
    }

    fn rebind_session_matched_target(
        config: &mut InstallConfig,
        marker_partition: &str,
    ) -> Result<String> {
        let marker_partition = Self::normalize_partition(marker_partition);
        if marker_partition.is_empty() {
            anyhow::bail!("安装标记所在分区无效，已中止");
        }

        let configured_target = Self::normalize_partition(&config.target_partition);
        if !configured_target.is_empty() && configured_target != marker_partition {
            log::info!(
                "[CONFIG] PE 盘符已变化，SessionId 精确匹配后将目标分区从 {} 重绑定为 {}",
                configured_target,
                marker_partition
            );
        }
        config.target_partition.clone_from(&marker_partition);
        Ok(marker_partition)
    }

    fn read_marker_value(content: &str, key: &str) -> String {
        content
            .lines()
            .filter_map(|line| line.trim().split_once('='))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case(key))
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default()
    }

    fn collect_install_markers() -> Vec<InstallMarker> {
        Self::scan_letters()
            .filter_map(|letter| {
                let partition = format!("{}:", letter);
                let marker_path = format!("{}\\{}", partition, Self::INSTALL_MARKER);
                let content = std::fs::read_to_string(marker_path).ok()?;
                Some(InstallMarker {
                    partition,
                    session_id: Self::read_marker_value(&content, "SessionId"),
                })
            })
            .collect()
    }

    fn collect_install_configs() -> Result<Vec<(String, InstallConfig)>> {
        let mut configs = Vec::new();
        for letter in Self::scan_letters() {
            let partition = format!("{}:", letter);
            let config_path = format!(
                "{}\\{}\\{}",
                partition,
                Self::DATA_DIR,
                Self::INSTALL_CONFIG
            );
            if Path::new(&config_path).exists() {
                configs.push((partition.clone(), Self::read_install_config(&partition)?));
            }
        }
        Ok(configs)
    }

    /// Binds the compatibility PE entry to exactly one marker/config pair. Old handoffs without a
    /// SessionId remain accepted only when there is one unambiguous marker and one config.
    pub fn find_install_task() -> Result<(String, String, InstallConfig)> {
        let markers = Self::collect_install_markers();
        let configs = Self::collect_install_configs()?;
        if markers.is_empty() || configs.is_empty() {
            anyhow::bail!("未找到完整的安装标记和配置文件");
        }

        let mut exact_matches = Vec::new();
        for marker in &markers {
            if marker.session_id.is_empty() {
                continue;
            }
            for (data_partition, config) in &configs {
                if !config.session_id.is_empty() && config.session_id == marker.session_id {
                    exact_matches.push((
                        data_partition.clone(),
                        marker.partition.clone(),
                        config.clone(),
                    ));
                }
            }
        }

        let (data_partition, target_partition, config) = if exact_matches.len() == 1 {
            let (data_partition, marker_partition, mut config) = exact_matches.remove(0);
            let target_partition =
                Self::rebind_session_matched_target(&mut config, &marker_partition)?;
            (data_partition, target_partition, config)
        } else if exact_matches.len() > 1 {
            anyhow::bail!("发现多个相同 SessionId 的安装任务，已中止");
        } else if markers.len() == 1 && configs.len() == 1 {
            let marker = markers.into_iter().next().expect("one marker");
            let (data_partition, config) = configs.into_iter().next().expect("one config");
            if !marker.session_id.is_empty()
                && !config.session_id.is_empty()
                && marker.session_id != config.session_id
            {
                anyhow::bail!("安装标记与配置的 SessionId 不一致，已中止");
            }
            (data_partition, marker.partition, config)
        } else {
            anyhow::bail!("发现多个安装标记或配置文件，无法确认本次任务，已中止");
        };

        let configured_target = Self::normalize_partition(&config.target_partition);
        if !configured_target.is_empty() && configured_target != target_partition {
            anyhow::bail!(
                "安装配置目标分区({configured_target})与标记分区({target_partition})不一致，已中止"
            );
        }
        Ok((data_partition, target_partition, config))
    }

    /// 查找包含安装标记文件的分区
    pub fn find_install_marker_partition() -> Option<String> {
        for letter in Self::scan_letters() {
            let marker_path = format!("{}:\\{}", letter, Self::INSTALL_MARKER);
            if Path::new(&marker_path).exists() {
                return Some(format!("{}:", letter));
            }
        }
        None
    }

    /// 查找包含备份标记文件的分区
    pub fn find_backup_marker_partition() -> Option<String> {
        for letter in Self::scan_letters() {
            let marker_path = format!("{}:\\{}", letter, Self::BACKUP_MARKER);
            if Path::new(&marker_path).exists() {
                return Some(format!("{}:", letter));
            }
        }
        None
    }

    /// 查找包含配置文件的数据分区
    pub fn find_data_partition() -> Option<String> {
        for letter in Self::scan_letters() {
            let config_path = format!("{}:\\{}\\{}", letter, Self::DATA_DIR, Self::INSTALL_CONFIG);
            if Path::new(&config_path).exists() {
                return Some(format!("{}:", letter));
            }
            let backup_config_path =
                format!("{}:\\{}\\{}", letter, Self::DATA_DIR, Self::BACKUP_CONFIG);
            if Path::new(&backup_config_path).exists() {
                return Some(format!("{}:", letter));
            }
        }
        None
    }

    /// 写入安装配置
    pub fn write_install_config(
        target_partition: &str,
        data_partition: &str,
        config: &InstallConfig,
    ) -> Result<()> {
        let mut config = config.clone();
        if config.session_id.trim().is_empty() {
            config.session_id = Self::new_session_id();
        }
        Self::validate_ini_value("target_partition", target_partition)?;
        Self::validate_ini_value("data_partition", data_partition)?;
        Self::validate_install_ini_values(&config)?;

        // 创建数据目录
        let data_dir = format!("{}\\{}", data_partition, Self::DATA_DIR);
        std::fs::create_dir_all(&data_dir).context(tr!("创建数据目录失败"))?;

        // 写入标记文件到目标分区
        let marker_path = format!("{}\\{}", target_partition, Self::INSTALL_MARKER);
        let marker_content = format!(
            "LetRecovery Install Marker\r\nSessionId={}\r\nTargetPartition={}\r\nDataPartition={}\r\n",
            config.session_id,
            target_partition,
            data_partition
        );
        std::fs::write(&marker_path, marker_content).context(tr!("写入安装标记文件失败"))?;

        // 处理自定义无人值守文件：把用户选择的 XML 复制到数据目录，INI 里只存相对文件名
        if !config.custom_unattend_path.is_empty() {
            const CUSTOM_UNATTEND_NAME: &str = "custom_unattend.xml";
            let dst = format!("{}\\{}", data_dir, CUSTOM_UNATTEND_NAME);
            std::fs::copy(&config.custom_unattend_path, &dst).with_context(|| {
                tr!(
                    "复制自定义无人值守文件失败: {}",
                    config.custom_unattend_path
                )
            })?;
            config.custom_unattend_path = CUSTOM_UNATTEND_NAME.to_string();
            log::info!("[CONFIG] 已复制自定义无人值守文件 -> {}", dst);
        }

        // 暂存 diskpart 脚本到数据目录，供重启进 PE 后执行（程序目录\bin\diskpart\ -> 数据目录\diskpart\）
        if config.run_diskpart_scripts {
            let src = crate::utils::path::get_diskpart_scripts_dir();
            let dst = format!("{}\\diskpart", data_dir);
            if src.exists() {
                if let Err(e) = copy_dir_recursive(&src, std::path::Path::new(&dst)) {
                    log::warn!("[CONFIG] 暂存 diskpart 脚本失败: {}", e);
                } else {
                    log::info!("[CONFIG] 已暂存 diskpart 脚本 -> {}", dst);
                }
            } else {
                log::info!(
                    "[CONFIG] 程序目录无 diskpart 文件夹，跳过暂存: {}",
                    src.display()
                );
            }
        }

        // 写入配置文件
        let config_path = format!("{}\\{}", data_dir, Self::INSTALL_CONFIG);
        let content = Self::serialize_install_config(&config);
        std::fs::write(&config_path, &content).context(tr!("写入安装配置文件失败"))?;

        log::info!("[CONFIG] 安装配置已写入: {}", config_path);
        log::info!("[CONFIG] 安装标记已写入: {}", marker_path);

        Ok(())
    }

    /// 写入无损扩容配置：在目标分区写 marker，在数据分区的数据目录写 ini。
    /// 扩容不格式化目标分区，故 data_partition 可直接用目标分区本身（如 "C:"）。
    pub fn write_expand_config(
        target_partition: &str,
        data_partition: &str,
        config: &ExpandConfig,
    ) -> Result<()> {
        let _transaction =
            Self::write_expand_config_transactional(target_partition, data_partition, config)?;
        Ok(())
    }

    pub fn write_expand_config_transactional(
        target_partition: &str,
        data_partition: &str,
        config: &ExpandConfig,
    ) -> Result<ExpandConfigTransaction> {
        Self::validate_ini_value("target_partition", target_partition)?;
        Self::validate_ini_value("data_partition", data_partition)?;
        Self::validate_ini_value("TargetPartition", &config.target_partition)?;
        let data_dir = PathBuf::from(format!("{}\\{}", data_partition, Self::DATA_DIR));
        let data_dir_created = !data_dir.exists();
        std::fs::create_dir_all(&data_dir).context(tr!("创建数据目录失败"))?;

        let marker_path = PathBuf::from(format!("{}\\{}", target_partition, Self::EXPAND_MARKER));
        let config_path = data_dir.join(Self::EXPAND_CONFIG);
        let marker_previous = if marker_path.exists() {
            Some(std::fs::read(&marker_path).context(tr!("读取原扩容标记文件失败"))?)
        } else {
            None
        };
        let config_previous = if config_path.exists() {
            Some(std::fs::read(&config_path).context(tr!("读取原扩容配置文件失败"))?)
        } else {
            None
        };
        let transaction = ExpandConfigTransaction {
            marker_path: marker_path.clone(),
            marker_previous,
            config_path: config_path.clone(),
            config_previous,
            data_dir,
            data_dir_created,
        };

        let content = format!(
            "[Expand]\r\nTargetPartition={}\r\nTargetSizeMb={}\r\nWimEngine={}\r\nLanguage={}\r\n",
            config.target_partition,
            config.target_size_mb,
            config.wim_engine,
            crate::utils::i18n::current_language()
        );
        if let Err(error) = write_atomic_file(&config_path, content.as_bytes()) {
            let _ = transaction.rollback();
            return Err(error).context(tr!("写入扩容配置文件失败"));
        }
        if let Err(error) = write_atomic_file(&marker_path, b"LetRecovery Expand Marker") {
            let _ = transaction.rollback();
            return Err(error).context(tr!("写入扩容标记文件失败"));
        }

        log::info!("[CONFIG] 扩容配置已写入: {}", config_path.display());
        log::info!("[CONFIG] 扩容标记已写入: {}", marker_path.display());
        Ok(transaction)
    }

    /// 写入备份配置
    pub fn write_backup_config(
        source_partition: &str,
        data_partition: &str,
        config: &BackupConfig,
    ) -> Result<()> {
        let _transaction =
            Self::write_backup_config_transactional(source_partition, data_partition, config)?;
        Ok(())
    }

    pub fn write_backup_config_transactional(
        source_partition: &str,
        data_partition: &str,
        config: &BackupConfig,
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
        let data_dir = PathBuf::from(format!("{}\\{}", data_partition, Self::DATA_DIR));
        let data_dir_created = !data_dir.exists();
        std::fs::create_dir_all(&data_dir).context(tr!("创建数据目录失败"))?;

        let marker_path = PathBuf::from(format!("{}\\{}", source_partition, Self::BACKUP_MARKER));
        let config_path = data_dir.join(Self::BACKUP_CONFIG);
        let marker_previous = if marker_path.exists() {
            Some(std::fs::read(&marker_path).context(tr!("读取原备份标记文件失败"))?)
        } else {
            None
        };
        let config_previous = if config_path.exists() {
            Some(std::fs::read(&config_path).context(tr!("读取原备份配置文件失败"))?)
        } else {
            None
        };
        let transaction = BackupConfigTransaction {
            marker_path: marker_path.clone(),
            marker_previous,
            config_path: config_path.clone(),
            config_previous,
            data_dir,
            data_dir_created,
        };

        let content = Self::serialize_backup_config(config);
        if let Err(error) = write_atomic_file(&config_path, content.as_bytes()) {
            let _ = transaction.rollback();
            return Err(error).context(tr!("写入备份配置文件失败"));
        }
        if let Err(error) = write_atomic_file(&marker_path, b"LetRecovery Backup Marker") {
            let _ = transaction.rollback();
            return Err(error).context(tr!("写入备份标记文件失败"));
        }

        log::info!("[CONFIG] 备份配置已写入: {}", config_path.display());
        log::info!("[CONFIG] 备份标记已写入: {}", marker_path.display());
        Ok(transaction)
    }

    /// 读取安装配置
    pub fn read_install_config(data_partition: &str) -> Result<InstallConfig> {
        let config_path = format!(
            "{}\\{}\\{}",
            data_partition,
            Self::DATA_DIR,
            Self::INSTALL_CONFIG
        );
        let content = std::fs::read_to_string(&config_path).context(tr!("读取安装配置文件失败"))?;
        Self::deserialize_install_config(&content)
    }

    /// 读取备份配置
    pub fn read_backup_config(data_partition: &str) -> Result<BackupConfig> {
        let config_path = format!(
            "{}\\{}\\{}",
            data_partition,
            Self::DATA_DIR,
            Self::BACKUP_CONFIG
        );
        let content = std::fs::read_to_string(&config_path).context(tr!("读取备份配置文件失败"))?;
        Self::deserialize_backup_config(&content)
    }

    /// 清理所有分区上的标记和配置文件
    pub fn cleanup_all_markers() {
        for letter in Self::scan_letters() {
            let _ = std::fs::remove_file(format!("{}:\\{}", letter, Self::INSTALL_MARKER));
            let _ = std::fs::remove_file(format!("{}:\\{}", letter, Self::BACKUP_MARKER));
            let _ = std::fs::remove_dir_all(format!("{}:\\{}", letter, Self::DATA_DIR));
            let _ = std::fs::remove_dir_all(format!("{}:\\{}", letter, Self::PE_DIR));
        }
    }

    /// 清理指定分区上的标记文件
    pub fn cleanup_partition_markers(partition: &str) {
        let _ = std::fs::remove_file(format!("{}\\{}", partition, Self::INSTALL_MARKER));
        let _ = std::fs::remove_file(format!("{}\\{}", partition, Self::BACKUP_MARKER));
    }

    /// 查找并清理自动创建的分区
    /// 返回被清理的分区盘符（如果有的话）
    pub fn cleanup_auto_created_partitions() -> Vec<char> {
        let mut cleaned = Vec::new();

        for letter in b'A'..=b'Z' {
            let c = letter as char;
            let marker_path = format!("{}:\\{}", c, Self::AUTO_CREATED_PARTITION_MARKER);

            if Path::new(&marker_path).exists() {
                log::info!("[CONFIG] 发现自动创建的分区: {}:", c);

                // 尝试删除分区
                if crate::core::disk::DiskManager::delete_auto_created_partition(c).is_ok() {
                    cleaned.push(c);
                    log::info!("[CONFIG] 已清理自动创建的分区: {}:", c);
                } else {
                    log::warn!("[CONFIG] 清理自动创建的分区失败: {}:", c);
                }
            }
        }

        cleaned
    }

    /// 检查指定分区是否是自动创建的
    pub fn is_auto_created_partition(partition: &str) -> bool {
        let letter = partition.chars().next().unwrap_or('X');
        let marker_path = format!("{}:\\{}", letter, Self::AUTO_CREATED_PARTITION_MARKER);
        Path::new(&marker_path).exists()
    }

    /// 获取数据目录路径
    pub fn get_data_dir(partition: &str) -> String {
        format!("{}\\{}", partition, Self::DATA_DIR)
    }

    /// 获取PE目录路径
    pub fn get_pe_dir(partition: &str) -> String {
        format!("{}\\{}", partition, Self::PE_DIR)
    }

    /// 序列化安装配置为INI格式
    fn serialize_install_config(config: &InstallConfig) -> String {
        format!(
            r#"[Install]
SessionId={}
Unattended={}
RestoreDrivers={}
DriverActionMode={}
AutoReboot={}
OriginalGUID={}
VolumeIndex={}
TargetPartition={}
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
VolumeLabel={}
CustomUnattendFile={}

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
            config.original_guid,
            config.volume_index,
            config.target_partition,
            config.image_path,
            config.is_gho,
            config.wim_engine,
            config.is_xp,
            config.is_xp_i386,
            config.xp_source_arch,
            config.run_diskpart_scripts,
            config.boot_mode,
            config.boot_pca_mode.as_config_value(),
            config.pca_compat_package,
            config.pca_compat_sha256,
            config.pca_compat_image_index,
            config.pca_compat_target_build,
            config.pca_compat_target_architecture,
            crate::utils::i18n::current_language(),
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
            config.volume_label,
            config.custom_unattend_path,
            config.win7_uefi_patch,
            config.win7_inject_usb3_driver,
            config.win7_inject_nvme_driver,
            config.win7_fix_acpi_bsod,
            config.win7_fix_storage_bsod,
            config.xp_inject_usb3_driver,
            config.xp_inject_nvme_driver,
        )
    }

    /// 序列化备份配置为INI格式
    fn serialize_backup_config(config: &BackupConfig) -> String {
        format!(
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
        )
    }

    /// 反序列化安装配置
    fn deserialize_install_config(content: &str) -> Result<InstallConfig> {
        let mut config = InstallConfig::default();

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
                    "OriginalGUID" => config.original_guid = value.to_string(),
                    "VolumeIndex" => config.volume_index = value.parse().unwrap_or(1),
                    "TargetPartition" => config.target_partition = value.to_string(),
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
                    "VolumeLabel" => config.volume_label = value.to_string(),
                    "CustomUnattendFile" => config.custom_unattend_path = value.to_string(),
                    "Win7UefiPatch" => config.win7_uefi_patch = value.parse().unwrap_or(false),
                    "Win7InjectUsb3Driver" => {
                        config.win7_inject_usb3_driver = value.parse().unwrap_or(false)
                    }
                    "Win7InjectNvmeDriver" => {
                        config.win7_inject_nvme_driver = value.parse().unwrap_or(false)
                    }
                    "Win7FixAcpiBsod" => config.win7_fix_acpi_bsod = value.parse().unwrap_or(false),
                    "Win7FixStorageBsod" => {
                        config.win7_fix_storage_bsod = value.parse().unwrap_or(false)
                    }
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

        Ok(config)
    }

    /// 反序列化备份配置
    fn deserialize_backup_config(content: &str) -> Result<BackupConfig> {
        let mut config = BackupConfig::default();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "SavePath" => config.save_path = value.to_string(),
                    "Name" => config.name = value.to_string(),
                    "Description" => config.description = value.to_string(),
                    "SourcePartition" => config.source_partition = value.to_string(),
                    "Incremental" => config.incremental = value.parse().unwrap_or(false),
                    "Format" => config.format = value.parse().unwrap_or(0),
                    "SwmSplitSize" => config.swm_split_size = value.parse().unwrap_or(4096),
                    "WimEngine" => config.wim_engine = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }

        Ok(config)
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
        assert_eq!(config.boot_mode, 0);
        assert_eq!(config.boot_pca_mode, BootPcaMode::Auto);
        assert!(!config.is_xp_i386);
        assert!(config.xp_source_arch.is_empty());
    }

    #[test]
    fn session_match_rebinds_target_to_the_marker_current_drive_letter() {
        let mut config = InstallConfig {
            session_id: "LR-session".to_string(),
            target_partition: "C:".to_string(),
            ..InstallConfig::default()
        };

        let target = ConfigFileManager::rebind_session_matched_target(&mut config, "e:\\").unwrap();

        assert_eq!(target, "E:");
        assert_eq!(config.target_partition, "E:");
    }

    #[test]
    fn install_config_round_trips_boot_selection() {
        let source = InstallConfig {
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

        let serialized = ConfigFileManager::serialize_install_config(&source);
        let parsed = ConfigFileManager::deserialize_install_config(&serialized).unwrap();

        assert_eq!(parsed.boot_mode, 1);
        assert_eq!(parsed.boot_pca_mode, BootPcaMode::Pca2023);
        assert_eq!(parsed.pca_compat_package, "pca_compat\\package.wim");
        assert_eq!(parsed.pca_compat_sha256, "a".repeat(64));
        assert_eq!(parsed.pca_compat_image_index, 1);
        assert_eq!(parsed.pca_compat_target_build, 19045);
        assert_eq!(parsed.pca_compat_target_architecture, 9);
        assert!(parsed.is_xp_i386);
        assert_eq!(parsed.xp_source_arch, "I386");
    }

    #[test]
    fn backup_transaction_restores_existing_files_and_preserves_unrelated_data() {
        let root = unique_temp_root("backup-restore");
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = root.join(ConfigFileManager::BACKUP_MARKER);
        let config_path = data_dir.join(ConfigFileManager::BACKUP_CONFIG);
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
                incremental: false,
                format: 0,
                swm_split_size: 4096,
                wim_engine: 0,
            },
        )
        .unwrap();
        assert_ne!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_ne!(std::fs::read(&config_path).unwrap(), b"old config");

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
            },
        )
        .unwrap();
        let marker = root.join(ConfigFileManager::BACKUP_MARKER);
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        let config_path = data_dir.join(ConfigFileManager::BACKUP_CONFIG);
        assert!(marker.exists());
        assert!(config_path.exists());

        transaction.rollback().unwrap();
        assert!(!marker.exists());
        assert!(!config_path.exists());
        assert!(!data_dir.exists());
        assert!(root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_transaction_restores_existing_files_and_preserves_unrelated_data() {
        let root = unique_temp_root("expand-restore");
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = root.join(ConfigFileManager::EXPAND_MARKER);
        let config_path = data_dir.join(ConfigFileManager::EXPAND_CONFIG);
        let unrelated = data_dir.join("user-owned.txt");
        std::fs::write(&marker, b"old marker").unwrap();
        std::fs::write(&config_path, b"old config").unwrap();
        std::fs::write(&unrelated, b"keep me").unwrap();

        let partition = root.to_string_lossy();
        let transaction = ConfigFileManager::write_expand_config_transactional(
            &partition,
            &partition,
            &ExpandConfig {
                target_partition: "C:".to_owned(),
                target_size_mb: 123_456,
                wim_engine: 1,
            },
        )
        .unwrap();
        assert_ne!(std::fs::read(&marker).unwrap(), b"old marker");
        assert_ne!(std::fs::read(&config_path).unwrap(), b"old config");

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
                target_partition: "C:".to_owned(),
                target_size_mb: 0,
                wim_engine: 0,
            },
        )
        .unwrap();
        let marker = root.join(ConfigFileManager::EXPAND_MARKER);
        let data_dir = root.join(ConfigFileManager::DATA_DIR);
        let config_path = data_dir.join(ConfigFileManager::EXPAND_CONFIG);
        assert!(marker.exists());
        assert!(config_path.exists());

        transaction.rollback().unwrap();
        assert!(!marker.exists());
        assert!(!config_path.exists());
        assert!(!data_dir.exists());
        assert!(root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
