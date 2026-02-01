use anyhow::{Context, Result};
use std::path::Path;

/// 系统安装配置（用于PE环境内安装）
#[derive(Debug, Clone, Default)]
pub struct InstallConfig {
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
}

impl InstallConfig {
    /// 根据DriverAction获取driver_action_mode值
    pub fn driver_action_to_mode(action: crate::app::DriverAction) -> u8 {
        match action {
            crate::app::DriverAction::None => 0,
            crate::app::DriverAction::SaveOnly => 1,
            crate::app::DriverAction::AutoImport => 2,
        }
    }
    
    /// 从driver_action_mode获取DriverAction
    pub fn mode_to_driver_action(mode: u8) -> crate::app::DriverAction {
        match mode {
            0 => crate::app::DriverAction::None,
            1 => crate::app::DriverAction::SaveOnly,
            2 => crate::app::DriverAction::AutoImport,
            // 兼容旧版本：如果restore_drivers为true则默认AutoImport
            _ => crate::app::DriverAction::AutoImport,
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

    /// 自动创建分区的标志文件名（与 disk.rs 中的常量保持一致）
    const AUTO_CREATED_PARTITION_MARKER: &'static str = "LetRecovery_AutoCreated.marker";

    /// 查找包含安装标记文件的分区
    pub fn find_install_marker_partition() -> Option<String> {
        for letter in ['C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K'] {
            let marker_path = format!("{}:\\{}", letter, Self::INSTALL_MARKER);
            if Path::new(&marker_path).exists() {
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
                return Some(format!("{}:", letter));
            }
            let backup_config_path = format!("{}:\\{}\\{}", letter, Self::DATA_DIR, Self::BACKUP_CONFIG);
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
        // 创建数据目录
        let data_dir = format!("{}\\{}", data_partition, Self::DATA_DIR);
        std::fs::create_dir_all(&data_dir)
            .context("创建数据目录失败")?;

        // 写入标记文件到目标分区
        let marker_path = format!("{}\\{}", target_partition, Self::INSTALL_MARKER);
        std::fs::write(&marker_path, "LetRecovery Install Marker")
            .context("写入安装标记文件失败")?;

        // 写入配置文件
        let config_path = format!("{}\\{}", data_dir, Self::INSTALL_CONFIG);
        let content = Self::serialize_install_config(config);
        std::fs::write(&config_path, &content)
            .context("写入安装配置文件失败")?;

        println!("[CONFIG] 安装配置已写入: {}", config_path);
        println!("[CONFIG] 安装标记已写入: {}", marker_path);

        Ok(())
    }

    /// 写入备份配置
    pub fn write_backup_config(
        source_partition: &str,
        data_partition: &str,
        config: &BackupConfig,
    ) -> Result<()> {
        // 创建数据目录
        let data_dir = format!("{}\\{}", data_partition, Self::DATA_DIR);
        std::fs::create_dir_all(&data_dir)
            .context("创建数据目录失败")?;

        // 写入标记文件到源分区
        let marker_path = format!("{}\\{}", source_partition, Self::BACKUP_MARKER);
        std::fs::write(&marker_path, "LetRecovery Backup Marker")
            .context("写入备份标记文件失败")?;

        // 写入配置文件
        let config_path = format!("{}\\{}", data_dir, Self::BACKUP_CONFIG);
        let content = Self::serialize_backup_config(config);
        std::fs::write(&config_path, &content)
            .context("写入备份配置文件失败")?;

        println!("[CONFIG] 备份配置已写入: {}", config_path);
        println!("[CONFIG] 备份标记已写入: {}", marker_path);

        Ok(())
    }

    /// 读取安装配置
    pub fn read_install_config(data_partition: &str) -> Result<InstallConfig> {
        let config_path = format!("{}\\{}\\{}", data_partition, Self::DATA_DIR, Self::INSTALL_CONFIG);
        let content = std::fs::read_to_string(&config_path)
            .context("读取安装配置文件失败")?;
        Self::deserialize_install_config(&content)
    }

    /// 读取备份配置
    pub fn read_backup_config(data_partition: &str) -> Result<BackupConfig> {
        let config_path = format!("{}\\{}\\{}", data_partition, Self::DATA_DIR, Self::BACKUP_CONFIG);
        let content = std::fs::read_to_string(&config_path)
            .context("读取备份配置文件失败")?;
        Self::deserialize_backup_config(&content)
    }

    /// 清理所有分区上的标记和配置文件
    pub fn cleanup_all_markers() {
        for letter in ['C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K'] {
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
                println!("[CONFIG] 发现自动创建的分区: {}:", c);
                
                // 尝试删除分区
                if let Ok(_) = crate::core::disk::DiskManager::delete_auto_created_partition(c) {
                    cleaned.push(c);
                    println!("[CONFIG] 已清理自动创建的分区: {}:", c);
                } else {
                    println!("[CONFIG] 清理自动创建的分区失败: {}:", c);
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
Unattended={}
RestoreDrivers={}
DriverActionMode={}
AutoReboot={}
OriginalGUID={}
VolumeIndex={}
TargetPartition={}
ImagePath={}
IsGho={}

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

[Win7]
Win7UefiPatch={}
Win7InjectUsb3Driver={}
Win7InjectNvmeDriver={}
Win7FixAcpiBsod={}
Win7FixStorageBsod={}
"#,
            config.unattended,
            config.restore_drivers,
            config.driver_action_mode,
            config.auto_reboot,
            config.original_guid,
            config.volume_index,
            config.target_partition,
            config.image_path,
            config.is_gho,
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
            config.win7_uefi_patch,
            config.win7_inject_usb3_driver,
            config.win7_inject_nvme_driver,
            config.win7_fix_acpi_bsod,
            config.win7_fix_storage_bsod,
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
"#,
            config.save_path,
            config.name,
            config.description,
            config.source_partition,
            config.incremental,
            config.format,
            config.swm_split_size,
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
                    "Unattended" => config.unattended = value.parse().unwrap_or(false),
                    "RestoreDrivers" => config.restore_drivers = value.parse().unwrap_or(false),
                    "DriverActionMode" => config.driver_action_mode = value.parse().unwrap_or(0),
                    "AutoReboot" => config.auto_reboot = value.parse().unwrap_or(false),
                    "OriginalGUID" => config.original_guid = value.to_string(),
                    "VolumeIndex" => config.volume_index = value.parse().unwrap_or(1),
                    "TargetPartition" => config.target_partition = value.to_string(),
                    "ImagePath" => config.image_path = value.to_string(),
                    "IsGho" => config.is_gho = value.parse().unwrap_or(false),
                    "RemoveShortcutArrow" => config.remove_shortcut_arrow = value.parse().unwrap_or(false),
                    "RestoreClassicContextMenu" => config.restore_classic_context_menu = value.parse().unwrap_or(false),
                    "BypassNRO" => config.bypass_nro = value.parse().unwrap_or(false),
                    "DisableWindowsUpdate" => config.disable_windows_update = value.parse().unwrap_or(false),
                    "DisableWindowsDefender" => config.disable_windows_defender = value.parse().unwrap_or(false),
                    "DisableReservedStorage" => config.disable_reserved_storage = value.parse().unwrap_or(false),
                    "DisableUAC" => config.disable_uac = value.parse().unwrap_or(false),
                    "DisableDeviceEncryption" => config.disable_device_encryption = value.parse().unwrap_or(false),
                    "RemoveUWPApps" => config.remove_uwp_apps = value.parse().unwrap_or(false),
                    "ImportStorageControllerDrivers" => config.import_storage_controller_drivers = value.parse().unwrap_or(false),
                    "CustomUsername" => config.custom_username = value.to_string(),
                    "VolumeLabel" => config.volume_label = value.to_string(),
                    "Win7UefiPatch" => config.win7_uefi_patch = value.parse().unwrap_or(false),
                    "Win7InjectUsb3Driver" => config.win7_inject_usb3_driver = value.parse().unwrap_or(false),
                    "Win7InjectNvmeDriver" => config.win7_inject_nvme_driver = value.parse().unwrap_or(false),
                    "Win7FixAcpiBsod" => config.win7_fix_acpi_bsod = value.parse().unwrap_or(false),
                    "Win7FixStorageBsod" => config.win7_fix_storage_bsod = value.parse().unwrap_or(false),
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
                    _ => {}
                }
            }
        }
        
        Ok(config)
    }
}
