//! 工具操作模块
//!
//! 提供各种工具的启动和操作功能

use std::process::Command;
use crate::utils::path::{get_bin_dir, get_tools_dir};

/// 启动指定工具
pub fn launch_tool(tool_name: &str) -> Result<(), String> {
    let tools_dir = get_tools_dir();
    let tool_path = tools_dir.join(tool_name);

    if tool_path.exists() {
        let result = if tool_name.to_lowercase().ends_with(".cpl") {
            Command::new("control.exe").arg(&tool_path).spawn()
        } else {
            Command::new(&tool_path).spawn()
        };

        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("启动失败: {} - {}", tool_name, e)),
        }
    } else {
        Err(format!("工具不存在: {:?}", tool_path))
    }
}

/// 启动Ghost工具
pub fn launch_ghost() -> Result<(), String> {
    let bin_dir = get_bin_dir();
    let ghost_path = bin_dir.join("ghost").join("Ghost64.exe");

    if ghost_path.exists() {
        match Command::new(&ghost_path).spawn() {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("启动失败: Ghost64.exe - {}", e)),
        }
    } else {
        Err(format!("工具不存在: {:?}", ghost_path))
    }
}

/// 启动万能驱动工具
pub fn launch_wandrv() -> Result<(), String> {
    let tools_dir = get_tools_dir();
    let wandrv_path = tools_dir.join("QDZC.exe");

    if wandrv_path.exists() {
        match Command::new(&wandrv_path).spawn() {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("启动失败: QDZC.exe - {}", e)),
        }
    } else {
        Err(format!("工具不存在: {:?}", wandrv_path))
    }
}

/// 修复引导
pub fn repair_boot(target_partition: &str) -> Result<(), String> {
    let boot_manager = crate::core::bcdedit::BootManager::new();
    boot_manager.repair_boot(target_partition)
        .map_err(|e| e.to_string())
}

/// 导出当前系统驱动
pub fn export_drivers(export_dir: &str) -> Result<(), String> {
    let dism = crate::core::dism::Dism::new();
    dism.export_drivers(export_dir)
        .map_err(|e| e.to_string())
}

/// 从指定分区导出驱动
pub fn export_drivers_from_partition(source_partition: &str, export_dir: &str) -> Result<(), String> {
    let dism = crate::core::dism::Dism::new();
    dism.export_drivers_from_system(source_partition, export_dir)
        .map_err(|e| e.to_string())
}
