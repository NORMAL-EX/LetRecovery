//! 离线登录修复
//!
//! 解决"还原镜像后进系统需要密码/出现『其他用户』"的问题。
//!
//! 背景：写入 `unattend.xml` 只对会经过 Windows Setup/OOBE 的镜像（已 sysprep 的
//! 安装镜像）生效；对"整盘备份/未 sysprep 的镜像"，OOBE 阶段根本不会运行，
//! 于是 unattend 里创建空密码账户与自动登录的设置全部失效，登录界面退化为
//! "其他用户"（需手动输入用户名+密码）。
//!
//! 这里分两层兜底：
//! 1) 零风险策略层（reg.exe load/unload，不动 SAM 二进制）：
//!    - SYSTEM：`Control\Lsa\LimitBlankPasswordUse = 0`，允许空密码账户用于
//!      自动登录/非控制台登录（默认被限制为 1）。
//!    - SOFTWARE：在已知目标用户名时配置 Winlogon 自动登录（空密码）。
//! 2) 非空密码清除层（仅在已知用户名时触发）：离线把目标账户在 SAM 中的 NT/LM
//!    hash 长度清零（等效空密码）并启用账户——该逻辑已收纳到共享库
//!    `lr_core::sam::clear_account_password`（含强制备份、成功后删除备份等安全措施）。
//!    该兜底只用于完整备份/未 sysprep 镜像；处于 reseal-to-OOBE 状态的安装镜像
//!    必须完全交给 unattend 创建账户，不能提前写 Winlogon 自动登录状态。

use anyhow::Result;
use std::path::Path;

use crate::core::registry::OfflineRegistry;
use crate::tr;

fn with_loaded_hive<T>(name: &str, path: &str, action: impl FnOnce() -> Result<T>) -> Result<T> {
    OfflineRegistry::load_hive(name, path)?;
    let action_result = action();
    let unload_result = OfflineRegistry::unload_hive(name);
    match (action_result, unload_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(action_error), Err(unload_error)) => anyhow::bail!(
            "{}; additionally failed to unload offline hive {name}: {unload_error}",
            action_error
        ),
    }
}

/// 离线 SYSTEM 配置单元在目标系统中的相对路径
fn system_hive_path(target_partition: &str) -> String {
    format!("{}\\Windows\\System32\\config\\SYSTEM", target_partition)
}

/// 离线 SOFTWARE 配置单元在目标系统中的相对路径
fn software_hive_path(target_partition: &str) -> String {
    format!("{}\\Windows\\System32\\config\\SOFTWARE", target_partition)
}

fn image_state_needs_legacy_login_fallback(image_state: &str) -> bool {
    image_state
        .trim()
        .eq_ignore_ascii_case("IMAGE_STATE_COMPLETE")
}

fn should_apply_legacy_login_fallback(target_partition: &str, force: bool) -> Result<bool> {
    if force {
        return Ok(true);
    }

    let software_hive = software_hive_path(target_partition);
    if !Path::new(&software_hive).exists() {
        anyhow::bail!("{}", tr!("目标 SOFTWARE 配置单元不存在: {}", software_hive));
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let hive_name = format!("LR_STATE_{}_{}", std::process::id(), nonce);
    let query_result = with_loaded_hive(&hive_name, &software_hive, || {
        let state_key = format!(
            "HKLM\\{}\\Microsoft\\Windows\\CurrentVersion\\Setup\\State",
            hive_name
        );
        OfflineRegistry::query_string(&state_key, "ImageState")
    });

    match query_result {
        Ok(image_state) => {
            let apply = image_state_needs_legacy_login_fallback(&image_state);
            log::info!(
                "[LOGIN] 目标镜像 ImageState={}，离线登录兜底={}",
                image_state,
                apply
            );
            Ok(apply)
        }
        Err(error) => {
            log::warn!(
                "[LOGIN] 无法确认目标镜像 ImageState，安全跳过离线自动登录兜底: {}",
                error
            );
            Ok(false)
        }
    }
}

/// 应用离线登录兜底设置。
///
/// - `target_partition`：目标系统盘，形如 `"C:"`。
/// - `username`：期望自动登录的用户名；为空时仅放开空密码策略，不配置自动登录
///   （避免对未知账户强行设置自动登录导致登录失败循环）。
///
/// `force_legacy_fallback` 仅供 GHO、XP/2003 等明确不会进入现代 OOBE 的路径使用。
/// 任一步失败都不会中断安装，调用方按需记录日志即可。
pub fn ensure_offline_login(
    target_partition: &str,
    username: &str,
    force_legacy_fallback: bool,
) -> Result<()> {
    if !should_apply_legacy_login_fallback(target_partition, force_legacy_fallback)? {
        log::info!("[LOGIN] 安装镜像将进入 OOBE，跳过离线 Winlogon/SAM 登录兜底");
        return Ok(());
    }

    let system_hive = system_hive_path(target_partition);
    let software_hive = software_hive_path(target_partition);

    if !Path::new(&system_hive).exists() {
        anyhow::bail!("{}", tr!("目标 SYSTEM 配置单元不存在: {}", system_hive));
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let system_name = format!("LR_SYS_{}_{}", std::process::id(), nonce);
    with_loaded_hive(&system_name, &system_hive, || {
        let select = format!("HKLM\\{}\\Select", system_name);
        let control_set = OfflineRegistry::query_dword(&select, "Current")
            .or_else(|_| OfflineRegistry::query_dword(&select, "Default"))?;
        if !(1..=999).contains(&control_set) {
            anyhow::bail!("offline SYSTEM Select contains invalid control set {control_set}");
        }
        let lsa = format!(
            "HKLM\\{}\\ControlSet{:03}\\Control\\Lsa",
            system_name, control_set
        );
        OfflineRegistry::set_dword(&lsa, "LimitBlankPasswordUse", 0)
    })?;

    // 2) SOFTWARE：仅在已知用户名时配置空密码自动登录
    if !username.is_empty() {
        if Path::new(&software_hive).exists() {
            let software_name = format!("LR_SOFT_{}_{}", std::process::id(), nonce);
            with_loaded_hive(&software_name, &software_hive, || {
                let winlogon = format!(
                    "HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon",
                    software_name
                );
                OfflineRegistry::create_key(&winlogon)?;
                OfflineRegistry::set_string(&winlogon, "AutoAdminLogon", "1")?;
                OfflineRegistry::set_string(&winlogon, "DefaultUserName", username)?;
                OfflineRegistry::set_string(&winlogon, "DefaultPassword", "")?;
                OfflineRegistry::set_dword(&winlogon, "AutoLogonCount", 1)
            })?;
        } else {
            log::warn!(
                "目标 SOFTWARE 配置单元不存在，跳过自动登录配置: {}",
                software_hive
            );
        }

        // 3) 离线清除该账户的非空密码（备份镜像里账户带密码时，让用户能空密码登录）。
        //    sysprep 镜像里该账户尚不存在 → 无匹配 → 安全空操作。复用共享库实现。
        if lr_core::sam::clear_account_password(target_partition, username)? {
            log::info!("[LOGIN] 已离线清除账户 [{}] 的密码", username);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::image_state_needs_legacy_login_fallback;

    #[test]
    fn only_complete_images_use_legacy_login_fallback() {
        assert!(image_state_needs_legacy_login_fallback(
            "IMAGE_STATE_COMPLETE"
        ));
        assert!(!image_state_needs_legacy_login_fallback(
            "IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE"
        ));
        assert!(!image_state_needs_legacy_login_fallback(
            "IMAGE_STATE_SPECIALIZE_RESEAL_TO_OOBE"
        ));
        assert!(!image_state_needs_legacy_login_fallback("UNKNOWN"));
    }
}
