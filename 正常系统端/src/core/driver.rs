//! Windows 驱动管理模块（核心实现已移入共享库 lr-core，此处再导出以保持调用方不变）。
//!
//! 正常系统端专属：离线驱动导入优先使用 dism.exe 命令行，失败再回退到
//! lr-core 的传统 Windows API 方法（`DriverManager::import_drivers_offline`）。

use std::path::Path;

use anyhow::Result;

use crate::tr;

// 共享的驱动类型从 lr-core 再导出（调用方无需改动）。
pub use lr_core::driver::{DriverInfo, DriverManager};

/// 离线驱动导入：优先使用 dism.exe，失败再回退到 lr-core 的传统方法。
///
/// 正常系统端专属逻辑。`manager` 用于在 dism 失败时回退调用
/// `manager.import_drivers_offline(...)`（lr-core 的传统 Windows API 实现）。
///
/// # 参数
/// - `offline_root`: 离线系统根目录 (如 "D:\\")
/// - `source_dir`: 驱动目录
///
/// # 返回
/// - (成功数, 失败数)
pub fn import_drivers_offline_dism_first(
    manager: &DriverManager,
    offline_root: &Path,
    source_dir: &Path,
) -> Result<(usize, usize)> {
    use crate::core::dism_cmd::DismCmd;

    log::info!(
        "[DriverManager] 使用 dism.exe 离线导入驱动: {:?} -> {:?}",
        source_dir, offline_root
    );

    // 规范化路径
    let image_path = offline_root.to_string_lossy();
    let driver_path = source_dir.to_string_lossy();

    // 使用 dism.exe 命令行进行离线驱动注入
    let dism_cmd = DismCmd::new()
        .map_err(|e| anyhow::anyhow!("{}", tr!("DISM 命令行初始化失败: {}", e)))?;

    // 统计驱动文件数量。与基线行为一致：源目录非法（不存在/非目录）时在此处
    // 通过 ? 提前返回错误，不再继续尝试 dism（find_inf_files 对非目录会 bail!）。
    let inf_count = DriverManager::find_inf_files(source_dir)?.len();

    // 使用智能导入（支持 INF 和 CAB）
    match dism_cmd.import_drivers_smart(&image_path, &driver_path, None) {
        Ok(_) => {
            log::info!(
                "[DriverManager] dism.exe 离线驱动导入成功"
            );
            // DISM 成功时假设所有驱动都导入成功
            Ok((inf_count.max(1), 0))
        }
        Err(e) => {
            log::warn!(
                "[DriverManager] dism.exe 导入失败: {}, 尝试备用方法",
                e
            );
            // 回退到传统方法（lr-core 的 Windows API 实现）
            manager.import_drivers_offline(offline_root, source_dir)
        }
    }
}

/// 导入驱动到离线系统（正常系统端：dism 优先，失败回退）。
///
/// # 参数
/// - `offline_root`: 离线系统根目录 (如 "D:\\")
/// - `driver_path`: 驱动目录
///
/// # 返回
/// - (成功数, 失败数)
pub fn import_drivers_offline(offline_root: &str, driver_path: &str) -> Result<(usize, usize)> {
    let manager = DriverManager::new()?;
    import_drivers_offline_dism_first(
        &manager,
        Path::new(offline_root),
        Path::new(driver_path),
    )
}
