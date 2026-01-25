//! 驱动功能模块
//!
//! 提供驱动备份、还原和导入功能

use std::path::Path;

/// 导出驱动到指定目录（离线系统）
pub fn export_drivers_offline(source_partition: &str, export_dir: &str) -> Result<(), String> {
    let dism = crate::core::dism::Dism::new();
    dism.export_drivers_from_system(source_partition, export_dir)
        .map_err(|e| e.to_string())
}

/// 导出当前系统驱动
pub fn export_drivers_online(export_dir: &str) -> Result<(), String> {
    let dism = crate::core::dism::Dism::new();
    dism.export_drivers(export_dir)
        .map_err(|e| e.to_string())
}

/// 导入驱动到离线系统
pub fn import_drivers_offline(target_partition: &str, driver_dir: &str) -> Result<(), String> {
    // 检查驱动目录是否存在
    if !Path::new(driver_dir).exists() {
        return Err(format!("驱动目录不存在: {}", driver_dir));
    }

    let dism = crate::core::dism::Dism::new();
    dism.add_drivers_offline(target_partition, driver_dir)
        .map_err(|e| e.to_string())
}

/// 获取存储控制器驱动目录
pub fn get_storage_driver_dir() -> Option<std::path::PathBuf> {
    let driver_dir = crate::utils::path::get_exe_dir()
        .join("drivers")
        .join("storage_controller");
    
    if driver_dir.exists() {
        Some(driver_dir)
    } else {
        None
    }
}

/// 导入存储控制器驱动到离线系统
pub fn import_storage_drivers(target_partition: &str) -> Result<(), String> {
    let driver_dir = get_storage_driver_dir()
        .ok_or_else(|| "存储控制器驱动目录不存在".to_string())?;
    
    import_drivers_offline(target_partition, &driver_dir.to_string_lossy())
}
