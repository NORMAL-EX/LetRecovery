//! 离线登录修复
//!
//! 解决"还原镜像后进系统需要密码/出现『其他用户』"的问题。
//!
//! 背景：写入 `unattend.xml` 只对会经过 Windows Setup/OOBE 的镜像（已 sysprep 的
//! 安装镜像）生效；对"整盘备份/未 sysprep 的镜像"，OOBE 阶段根本不会运行，
//! 于是 unattend 里创建空密码账户与自动登录的设置全部失效，登录界面退化为
//! "其他用户"（需手动输入用户名+密码）。
//!
//! 这里通过离线编辑目标系统的注册表（reg.exe load/unload，无需二进制改写 SAM，
//! 因此零损坏风险）来兜底：
//! - SYSTEM 配置单元：`Control\Lsa\LimitBlankPasswordUse = 0`
//!   允许空密码账户用于自动登录/非控制台登录（默认被限制为 1）。
//! - SOFTWARE 配置单元：在已知目标用户名时配置 Winlogon 自动登录（空密码）。
//!
//! 注：对"账户本身设有非空密码"的备份镜像，离线清除其密码需要二进制改写 SAM
//! （chntpw 方式），风险高，未在此实现；本模块只做零风险的策略与自动登录兜底。

use anyhow::Result;
use std::path::Path;

use crate::core::registry::OfflineRegistry;

/// 离线 SYSTEM 配置单元在目标系统中的相对路径
fn system_hive_path(target_partition: &str) -> String {
    format!("{}\\Windows\\System32\\config\\SYSTEM", target_partition)
}

/// 离线 SOFTWARE 配置单元在目标系统中的相对路径
fn software_hive_path(target_partition: &str) -> String {
    format!("{}\\Windows\\System32\\config\\SOFTWARE", target_partition)
}

/// 应用离线登录兜底设置。
///
/// - `target_partition`：目标系统盘，形如 `"C:"`。
/// - `username`：期望自动登录的用户名；为空时仅放开空密码策略，不配置自动登录
///   （避免对未知账户强行设置自动登录导致登录失败循环）。
///
/// 任一步失败都不会中断安装，调用方按需记录日志即可。
pub fn ensure_offline_login(target_partition: &str, username: &str) -> Result<()> {
    let system_hive = system_hive_path(target_partition);
    let software_hive = software_hive_path(target_partition);

    if !Path::new(&system_hive).exists() {
        anyhow::bail!("目标 SYSTEM 配置单元不存在: {}", system_hive);
    }

    // 1) SYSTEM：放开空密码使用限制（离线时控制集通常是 ControlSet001）
    if let Err(e) = OfflineRegistry::load_hive("LR_SYS", &system_hive) {
        anyhow::bail!("加载 SYSTEM 配置单元失败: {}", e);
    }
    let lsa_keys = [
        "HKLM\\LR_SYS\\ControlSet001\\Control\\Lsa",
        "HKLM\\LR_SYS\\ControlSet002\\Control\\Lsa",
    ];
    for k in &lsa_keys {
        // 键可能不存在（如只有 ControlSet001），失败忽略
        let _ = OfflineRegistry::set_dword(k, "LimitBlankPasswordUse", 0);
    }
    let _ = OfflineRegistry::unload_hive("LR_SYS");

    // 2) SOFTWARE：仅在已知用户名时配置空密码自动登录
    if !username.is_empty() {
        if Path::new(&software_hive).exists() {
            if let Err(e) = OfflineRegistry::load_hive("LR_SOFT", &software_hive) {
                anyhow::bail!("加载 SOFTWARE 配置单元失败: {}", e);
            }
            let winlogon = "HKLM\\LR_SOFT\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon";
            let _ = OfflineRegistry::create_key(winlogon);
            let _ = OfflineRegistry::set_string(winlogon, "AutoAdminLogon", "1");
            let _ = OfflineRegistry::set_string(winlogon, "DefaultUserName", username);
            let _ = OfflineRegistry::set_string(winlogon, "DefaultPassword", "");
            // 仅自动登录一次，登录后由用户自行设置（避免无限自动登录）
            let _ = OfflineRegistry::set_dword(winlogon, "AutoLogonCount", 1);
            let _ = OfflineRegistry::unload_hive("LR_SOFT");
        } else {
            log::warn!("目标 SOFTWARE 配置单元不存在，跳过自动登录配置: {}", software_hive);
        }
    }

    Ok(())
}
