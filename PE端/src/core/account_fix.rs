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
//! 2) 非空密码清除层（仅在已知用户名时触发，见 `clear_account_password`）：
//!    离线把目标账户在 SAM 中的 NT/LM hash「长度」清零（chntpw 思路），等效于
//!    把密码改为空。为降低风险：**操作前强制备份 SAM（成功收尾后删除，仅出错时保留）**、
//!    严格校验 V 结构、只覆盖 4 字节长度字段（不改 hive 结构 / 不挪动数据）；任何异常即跳过。
//!    sysprep 镜像里目标账户尚未创建 → 无匹配 → 自动空操作，故对装机无副作用。

use anyhow::Result;
use std::path::Path;

use crate::core::registry::OfflineRegistry;
use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;

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
            log::warn!(
                "目标 SOFTWARE 配置单元不存在，跳过自动登录配置: {}",
                software_hive
            );
        }

        // 3) 离线清除该账户的非空密码（备份镜像里账户带密码时，让用户能空密码登录）。
        //    sysprep 镜像里该账户尚不存在 → 无匹配 → 安全空操作。
        match clear_account_password(target_partition, username) {
            Ok(true) => log::info!("[LOGIN] 已离线清除账户 [{}] 的密码", username),
            Ok(false) => {}
            Err(e) => log::warn!("[LOGIN] 离线清除账户密码失败（不影响安装）: {}", e),
        }
    }

    Ok(())
}

/// 离线清除目标系统中指定账户的密码（把 SAM 中该用户 V 结构的 NT/LM hash 长度清零）。
///
/// - `username` 为空时直接返回 `Ok(false)`（不指定用户名不清除，避免误清整盘备份里的所有账户）。
/// - 返回 `Ok(true)` 表示确实清除了某账户的密码。
///
/// 安全措施：先把 SAM 复制为 `SAM.lrbak` 备份；只覆盖 V 结构里的 4 字节长度字段，
/// 不改动 hive 结构、不挪动数据；解析失败/越界一律跳过，绝不写回可疑数据。
/// **成功收尾后会删除该备份**（避免在目标系统留下含账户哈希的 SAM 副本）；仅在出错时保留备份以便恢复。
pub fn clear_account_password(target_partition: &str, username: &str) -> Result<bool> {
    let username = username.trim();
    if username.is_empty() {
        return Ok(false);
    }

    let sam_hive = format!("{}\\Windows\\System32\\config\\SAM", target_partition);
    if !Path::new(&sam_hive).exists() {
        anyhow::bail!("目标 SAM 配置单元不存在: {}", sam_hive);
    }

    // 强制备份：备份失败则绝不继续改 SAM
    let backup = format!("{}.lrbak", sam_hive);
    std::fs::copy(&sam_hive, &backup)
        .map_err(|e| anyhow::anyhow!("备份 SAM 失败，已放弃清除密码: {}", e))?;
    log::info!("[SAM] 已备份 SAM -> {}", backup);

    OfflineRegistry::load_hive("LR_SAM", &sam_hive)
        .map_err(|e| anyhow::anyhow!("加载 SAM 配置单元失败: {}", e))?;

    // 用闭包包裹，确保无论成功失败都能卸载 hive
    let result = (|| -> Result<bool> {
        let users_key = "HKLM\\LR_SAM\\SAM\\Domains\\Account\\Users";
        let rids = list_user_rids(users_key)?;
        let mut cleared = false;

        for rid in rids {
            let user_key = format!("{}\\{}", users_key, rid);
            let v = match reg_read_binary(&user_key, "V") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let name = match parse_v_username(&v) {
                Some(n) => n,
                None => continue,
            };
            if !name.eq_ignore_ascii_case(username) {
                continue;
            }

            // 清空 NT/LM hash 长度（等效空密码）
            let mut patched = v.clone();
            if blank_v_password(&mut patched) {
                reg_write_binary(&user_key, "V", &patched)?;
                log::info!("[SAM] 已清除账户 [{}] (RID {}) 的密码", name, rid);
                cleared = true;
            } else {
                log::info!("[SAM] 账户 [{}] 已是空密码，无需清除", name);
            }

            // 顺带启用被禁用的账户（清除 F 结构中的 ACB_DISABLED 位）
            if let Ok(f) = reg_read_binary(&user_key, "F") {
                if let Some(new_f) = enable_account_f(&f) {
                    if reg_write_binary(&user_key, "F", &new_f).is_ok() {
                        log::info!("[SAM] 已启用账户 [{}]", name);
                    }
                }
            }
        }
        Ok(cleared)
    })();

    let _ = OfflineRegistry::unload_hive("LR_SAM");

    if let Ok(false) = &result {
        log::info!("[SAM] 未找到匹配账户 [{}]，SAM 未改动", username);
    }

    // 收尾：成功（无论是否改动）即删除 SAM 备份，避免在目标系统永久留下含账户哈希的
    // SAM 副本（安全隐患）；仅在出错时保留备份，便于必要时手动恢复。
    match &result {
        Ok(_) => match std::fs::remove_file(&backup) {
            Ok(_) => log::info!("[SAM] 已删除临时备份 {}", backup),
            Err(e) => log::warn!("[SAM] 删除临时备份失败（可手动删除 {}）: {}", backup, e),
        },
        Err(_) => log::warn!("[SAM] 操作出错，保留 SAM 备份以便恢复: {}", backup),
    }

    result
}

/// 枚举 `Users` 键下的用户 RID 子键（8 位十六进制，如 000001F4）。
fn list_user_rids(users_key: &str) -> Result<Vec<String>> {
    let out = new_command("reg.exe").args(["query", users_key]).output()?;
    if !out.status.success() {
        anyhow::bail!("枚举 SAM 用户失败: {}", gbk_to_utf8(&out.stderr));
    }
    let text = gbk_to_utf8(&out.stdout);
    let mut rids = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.trim().rsplit('\\').next() {
            if name.len() == 8 && name.chars().all(|c| c.is_ascii_hexdigit()) {
                rids.push(name.to_string());
            }
        }
    }
    Ok(rids)
}

/// 读取注册表 REG_BINARY 值为字节数组。
fn reg_read_binary(key: &str, value: &str) -> Result<Vec<u8>> {
    let out = new_command("reg.exe")
        .args(["query", key, "/v", value])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("reg query 失败: {}", gbk_to_utf8(&out.stderr));
    }
    let text = gbk_to_utf8(&out.stdout);
    for line in text.lines() {
        if let Some(pos) = line.find("REG_BINARY") {
            let hex = line[pos + "REG_BINARY".len()..].trim();
            return hex_to_bytes(hex);
        }
    }
    anyhow::bail!("未找到 {} 的 REG_BINARY 值", value);
}

/// 写入注册表 REG_BINARY 值。
fn reg_write_binary(key: &str, value: &str, data: &[u8]) -> Result<()> {
    let hex: String = data.iter().map(|b| format!("{:02x}", b)).collect();
    let out = new_command("reg.exe")
        .args([
            "add",
            key,
            "/v",
            value,
            "/t",
            "REG_BINARY",
            "/d",
            &hex,
            "/f",
        ])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("reg add 失败: {}", gbk_to_utf8(&out.stderr));
    }
    Ok(())
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>> {
    let hex: Vec<u8> = s.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    if hex.len() % 2 != 0 {
        anyhow::bail!("十六进制长度异常");
    }
    let val = |c: u8| (c as char).to_digit(16).unwrap() as u8;
    Ok(hex
        .chunks_exact(2)
        .map(|c| (val(c[0]) << 4) | val(c[1]))
        .collect())
}

fn read_u32_le(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// 从 V 结构解析用户名（header 偏移 0x0c=用户名偏移、0x10=长度；数据区从 0xcc 起，UTF-16LE）。
fn parse_v_username(v: &[u8]) -> Option<String> {
    if v.len() < 0xcc {
        return None;
    }
    let uoff = read_u32_le(v, 0x0c)? as usize;
    let ulen = read_u32_le(v, 0x10)? as usize;
    if ulen == 0 {
        return None;
    }
    let start = 0xccusize.checked_add(uoff)?;
    let end = start.checked_add(ulen)?;
    if end > v.len() {
        return None;
    }
    let units: Vec<u16> = v[start..end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// 把 V 结构里的 LM(0xa0)/NT(0xac) hash 长度字段清零，等效空密码。返回是否有改动。
fn blank_v_password(v: &mut [u8]) -> bool {
    if v.len() < 0xcc {
        return false;
    }
    let mut changed = false;
    for &len_off in &[0xa0usize, 0xacusize] {
        if let Some(len) = read_u32_le(v, len_off) {
            if len != 0 {
                v[len_off..len_off + 4].copy_from_slice(&0u32.to_le_bytes());
                changed = true;
            }
        }
    }
    changed
}

/// 清除 F 结构中的 ACB_DISABLED 位（偏移 0x38 处的 USHORT 标志位），启用账户。
/// 返回修改后的 F；若账户本就启用则返回 None。
fn enable_account_f(f: &[u8]) -> Option<Vec<u8>> {
    if f.len() < 0x3a {
        return None;
    }
    let flags = u16::from_le_bytes([f[0x38], f[0x39]]);
    const ACB_DISABLED: u16 = 0x0001;
    if flags & ACB_DISABLED != 0 {
        let mut nf = f.to_vec();
        nf[0x38..0x3a].copy_from_slice(&(flags & !ACB_DISABLED).to_le_bytes());
        Some(nf)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成一个最小可解析的 SAM "V" 结构：
    /// header 偏移 0x0c=用户名相对偏移、0x10=用户名字节长度、0xa0=LM长度、0xac=NT长度，
    /// 用户名数据从 0xcc+uoff 起，UTF-16LE。
    fn build_v(username: &str, uoff: u32, lm_len: u32, nt_len: u32) -> Vec<u8> {
        let uname: Vec<u8> = username.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let data_start = 0xcc + uoff as usize;
        let mut v = vec![0u8; data_start + uname.len()];
        v[0x0c..0x10].copy_from_slice(&uoff.to_le_bytes());
        v[0x10..0x14].copy_from_slice(&(uname.len() as u32).to_le_bytes());
        v[0xa0..0xa4].copy_from_slice(&lm_len.to_le_bytes());
        v[0xac..0xb0].copy_from_slice(&nt_len.to_le_bytes());
        v[data_start..data_start + uname.len()].copy_from_slice(&uname);
        v
    }

    /// 合成一个最小 SAM "F" 结构：偏移 0x38 处的 USHORT 为账户控制标志位。
    fn build_f(flags: u16) -> Vec<u8> {
        let mut f = vec![0u8; 0x40];
        f[0x38..0x3a].copy_from_slice(&flags.to_le_bytes());
        f
    }

    #[test]
    fn hex_to_bytes_works() {
        assert_eq!(hex_to_bytes("dEadBeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        // 过滤空白等非十六进制字符
        assert_eq!(hex_to_bytes("de ad\tbe ef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert!(hex_to_bytes("abc").is_err()); // 奇数长度
        assert_eq!(hex_to_bytes("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn read_u32_le_bounds() {
        assert_eq!(read_u32_le(&[1, 0, 0, 0], 0), Some(1));
        assert_eq!(read_u32_le(&[0xff, 0xff, 0xff, 0xff], 0), Some(0xffff_ffff));
        assert_eq!(read_u32_le(&[1, 2, 3], 0), None); // 越界返回 None
    }

    #[test]
    fn parse_v_username_basic_and_offset() {
        assert_eq!(
            parse_v_username(&build_v("Administrator", 0, 16, 16)).as_deref(),
            Some("Administrator")
        );
        // 用户名数据带相对偏移、且含非 ASCII
        assert_eq!(parse_v_username(&build_v("用户A", 8, 16, 16)).as_deref(), Some("用户A"));
    }

    #[test]
    fn parse_v_username_edge_cases() {
        assert_eq!(parse_v_username(&[0u8; 0x80]), None); // 结构过短
        assert_eq!(parse_v_username(&build_v("", 0, 0, 0)), None); // 用户名长度 0
        // 用户名长度字段越界 → 安全返回 None（不 panic）
        let mut v = build_v("X", 0, 0, 0);
        v[0x10..0x14].copy_from_slice(&9999u32.to_le_bytes());
        assert_eq!(parse_v_username(&v), None);
    }

    #[test]
    fn blank_v_password_zeroes_hash_lengths() {
        let mut v = build_v("u", 0, 16, 16);
        assert!(blank_v_password(&mut v)); // 有非零长度 → 清零并返回 true
        assert_eq!(read_u32_le(&v, 0xa0), Some(0)); // LM 长度被清零
        assert_eq!(read_u32_le(&v, 0xac), Some(0)); // NT 长度被清零
        assert!(!blank_v_password(&mut v)); // 已是空密码 → 无改动
    }

    #[test]
    fn blank_v_password_noop_cases() {
        let mut v = build_v("u", 0, 0, 0);
        assert!(!blank_v_password(&mut v)); // 本就为 0
        assert!(!blank_v_password(&mut vec![0u8; 0x80])); // 结构过短
    }

    #[test]
    fn enable_account_f_clears_disabled_bit() {
        // 含 ACB_DISABLED(0x0001) + 其它位(0x0210) → 仅清掉 0x0001、保留其它
        let nf = enable_account_f(&build_f(0x0211)).expect("禁用账户应被改动");
        assert_eq!(u16::from_le_bytes([nf[0x38], nf[0x39]]), 0x0210);
        assert!(enable_account_f(&build_f(0x0210)).is_none()); // 本就启用 → None
        assert!(enable_account_f(&[0u8; 0x10]).is_none()); // 结构过短 → None
    }
}
