use anyhow::{Context, Result};
use std::path::Path;

/// 驱动操作模式
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DriverActionMode {
    /// 无操作
    #[default]
    None = 0,
    /// 仅保存驱动（到数据目录）
    SaveOnly = 1,
    /// 自动导入（保存并导入到新系统）
    AutoImport = 2,
}

impl DriverActionMode {
    /// 从数值转换
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::SaveOnly,
            2 => Self::AutoImport,
            _ => Self::None,
        }
    }
    
    /// 转换为数值
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    /// 是否需要导入驱动
    pub fn should_import(&self) -> bool {
        *self == Self::AutoImport
    }
    
    /// 是否有驱动目录（SaveOnly 或 AutoImport 时都有）
    pub fn has_drivers(&self) -> bool {
        *self != Self::None
    }
}

/// 系统安装配置（用于PE环境内安装）
#[derive(Debug, Clone, Default)]
pub struct InstallConfig {
    /// 无人值守安装
    pub unattended: bool,
    /// 驱动还原（兼容旧版本）
    pub restore_drivers: bool,
    /// 驱动操作模式: 0=无, 1=仅保存, 2=自动导入
    pub driver_action_mode: DriverActionMode,
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
    /// 禁用Windows更新
    pub disable_windows_update: bool,
    /// 禁用Windows安全中心
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
}

impl InstallConfig {
    /// 判断是否需要导入驱动
    /// 优先使用新的driver_action_mode，兼容旧的restore_drivers
    pub fn should_import_drivers(&self) -> bool {
        // 优先使用新的driver_action_mode
        if self.driver_action_mode != DriverActionMode::None {
            self.driver_action_mode.should_import()
        } else {
            // 兼容旧版本
            self.restore_drivers
        }
    }
    
    /// 判断是否有驱动目录需要处理
    pub fn has_driver_data(&self) -> bool {
        self.driver_action_mode.has_drivers() || self.restore_drivers
    }
}

/// 备份格式
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BackupFormat {
    #[default]
    Wim = 0,
    Esd = 1,
    Swm = 2,
    Gho = 3,
}

impl BackupFormat {
    /// 从数值转换
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Wim,
            1 => Self::Esd,
            2 => Self::Swm,
            3 => Self::Gho,
            _ => Self::Wim,
        }
    }
    
    /// 获取文件扩展名
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Wim => "wim",
            Self::Esd => "esd",
            Self::Swm => "swm",
            Self::Gho => "gho",
        }
    }
    
    /// 获取格式描述
    pub fn description(&self) -> &'static str {
        match self {
            Self::Wim => "WIM格式",
            Self::Esd => "ESD格式（高压缩）",
            Self::Swm => "SWM格式（分卷）",
            Self::Gho => "GHO格式（Ghost）",
        }
    }
}

/// 系统备份配置（用于PE环境内备份）
#[derive(Debug, Clone, Default)]
pub struct BackupConfig {
    /// 备份保存路径
    pub save_path: String,
    /// 备份名称
    pub name: String,
    /// 备份描述
    pub description: String,
    /// 源分区盘符
    pub source_partition: String,
    /// 是否增量备份
    pub incremental: bool,
    /// 备份格式
    pub format: BackupFormat,
    /// SWM分卷大小（MB）
    pub swm_split_size: u32,
}

/// 配置文件管理器
pub struct ConfigFileManager;

impl ConfigFileManager {
    /// 标记文件名
    const INSTALL_MARKER: &'static str = "LetRecovery_Install.marker";
    const BACKUP_MARKER: &'static str = "LetRecovery_Backup.marker";

    /// 配置文件名
    const INSTALL_CONFIG: &'static str = "LetRecovery_Install.ini";
    const BACKUP_CONFIG: &'static str = "LetRecovery_Backup.ini";

    /// PE文件目录名
    const PE_DIR: &'static str = "LetRecovery_PE";

    /// 临时数据目录名
    const DATA_DIR: &'static str = "LetRecovery_Data";

    /// 查找包含安装标记文件的分区
    pub fn find_install_marker_partition() -> Option<String> {
        for letter in ['C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K'] {
            let marker_path = format!("{}:\\{}", letter, Self::INSTALL_MARKER);
            if Path::new(&marker_path).exists() {
                log::info!("找到安装标记分区: {}:", letter);
                return Some(format!("{}:", letter));
            }
        }
        None
    }

    /// 查找包含备份标记文件的分区
    pub fn find_backup_marker_partition() -> Option<String> {
        for letter in ['C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K'] {
            let marker_path = format!("{}:\\{}", letter, Self::BACKUP_MARKER);
            if Path::new(&marker_path).exists() {
                log::info!("找到备份标记分区: {}:", letter);
                return Some(format!("{}:", letter));
            }
        }
        None
    }

    /// 查找包含配置文件的数据分区
    pub fn find_data_partition() -> Option<String> {
        for letter in ['C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K'] {
            let config_path = format!("{}:\\{}\\{}", letter, Self::DATA_DIR, Self::INSTALL_CONFIG);
            if Path::new(&config_path).exists() {
                log::info!("找到安装配置分区: {}:", letter);
                return Some(format!("{}:", letter));
            }
            let backup_config_path =
                format!("{}:\\{}\\{}", letter, Self::DATA_DIR, Self::BACKUP_CONFIG);
            if Path::new(&backup_config_path).exists() {
                log::info!("找到备份配置分区: {}:", letter);
                return Some(format!("{}:", letter));
            }
        }
        None
    }

    /// 检测操作类型 (安装或备份)
    pub fn detect_operation_type() -> Option<OperationType> {
        // 先检查安装标记
        if Self::find_install_marker_partition().is_some() {
            if let Some(data_part) = Self::find_data_partition() {
                let install_config_path = format!(
                    "{}\\{}\\{}",
                    data_part,
                    Self::DATA_DIR,
                    Self::INSTALL_CONFIG
                );
                if Path::new(&install_config_path).exists() {
                    return Some(OperationType::Install);
                }
            }
        }

        // 再检查备份标记
        if Self::find_backup_marker_partition().is_some() {
            if let Some(data_part) = Self::find_data_partition() {
                let backup_config_path =
                    format!("{}\\{}\\{}", data_part, Self::DATA_DIR, Self::BACKUP_CONFIG);
                if Path::new(&backup_config_path).exists() {
                    return Some(OperationType::Backup);
                }
            }
        }

        None
    }

    /// 读取安装配置
    pub fn read_install_config(data_partition: &str) -> Result<InstallConfig> {
        let config_path = format!(
            "{}\\{}\\{}",
            data_partition,
            Self::DATA_DIR,
            Self::INSTALL_CONFIG
        );
        log::info!("读取安装配置: {}", config_path);
        let content =
            std::fs::read_to_string(&config_path).context("读取安装配置文件失败")?;
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
        log::info!("读取备份配置: {}", config_path);
        let content =
            std::fs::read_to_string(&config_path).context("读取备份配置文件失败")?;
        Self::deserialize_backup_config(&content)
    }

    /// 获取数据目录路径
    pub fn get_data_dir(partition: &str) -> String {
        format!("{}\\{}", partition, Self::DATA_DIR)
    }

    /// 获取PE目录路径
    pub fn get_pe_dir(partition: &str) -> String {
        format!("{}\\{}", partition, Self::PE_DIR)
    }

    /// 清理指定分区上的标记文件
    pub fn cleanup_partition_markers(partition: &str) {
        let install_marker = format!("{}\\{}", partition, Self::INSTALL_MARKER);
        let backup_marker = format!("{}\\{}", partition, Self::BACKUP_MARKER);

        if let Err(e) = std::fs::remove_file(&install_marker) {
            log::debug!("删除安装标记失败 (可能不存在): {}", e);
        } else {
            log::info!("已删除安装标记: {}", install_marker);
        }

        if let Err(e) = std::fs::remove_file(&backup_marker) {
            log::debug!("删除备份标记失败 (可能不存在): {}", e);
        } else {
            log::info!("已删除备份标记: {}", backup_marker);
        }
    }

    /// 清理数据目录
    pub fn cleanup_data_dir(partition: &str) {
        let data_dir = Self::get_data_dir(partition);
        if let Err(e) = std::fs::remove_dir_all(&data_dir) {
            log::debug!("删除数据目录失败 (可能不存在): {}", e);
        } else {
            log::info!("已删除数据目录: {}", data_dir);
        }
    }

    /// 清理PE目录
    pub fn cleanup_pe_dir(partition: &str) {
        let pe_dir = Self::get_pe_dir(partition);
        if let Err(e) = std::fs::remove_dir_all(&pe_dir) {
            log::debug!("删除PE目录失败 (可能不存在): {}", e);
        } else {
            log::info!("已删除PE目录: {}", pe_dir);
        }
    }

    /// 清理所有临时文件
    pub fn cleanup_all(data_partition: &str, target_partition: &str) {
        Self::cleanup_partition_markers(target_partition);
        Self::cleanup_data_dir(data_partition);
        Self::cleanup_pe_dir(data_partition);
    }

    /// 反序列化安装配置
    fn deserialize_install_config(content: &str) -> Result<InstallConfig> {
        let mut config = InstallConfig::default();
        config.volume_index = 1; // 默认值

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "Unattended" => config.unattended = value.parse().unwrap_or(false),
                    "RestoreDrivers" => config.restore_drivers = value.parse().unwrap_or(false),
                    "DriverActionMode" => {
                        let mode_value: u8 = value.parse().unwrap_or(0);
                        config.driver_action_mode = DriverActionMode::from_u8(mode_value);
                    }
                    "AutoReboot" => config.auto_reboot = value.parse().unwrap_or(false),
                    "OriginalGUID" => config.original_guid = value.to_string(),
                    "VolumeIndex" => config.volume_index = value.parse().unwrap_or(1),
                    "TargetPartition" => config.target_partition = value.to_string(),
                    "ImagePath" => config.image_path = value.to_string(),
                    "IsGho" => config.is_gho = value.parse().unwrap_or(false),
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
                    _ => {}
                }
            }
        }

        Ok(config)
    }

    /// 反序列化备份配置
    fn deserialize_backup_config(content: &str) -> Result<BackupConfig> {
        let mut config = BackupConfig::default();
        config.swm_split_size = 4096; // 默认4GB

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
                    "Format" => {
                        let format_value: u8 = value.parse().unwrap_or(0);
                        config.format = BackupFormat::from_u8(format_value);
                    }
                    "SwmSplitSize" => config.swm_split_size = value.parse().unwrap_or(4096),
                    _ => {}
                }
            }
        }

        Ok(config)
    }
}

/// 操作类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OperationType {
    Install,
    Backup,
}
