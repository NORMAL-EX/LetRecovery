//! 离线 SAM 账户操作（两端共享）：清除指定账户密码、启用被禁用账户。
//!
//! 通过 `reg.exe load/unload` 挂载离线 SAM 配置单元，按 chntpw 思路把目标账户
//! 在 SAM `V` 结构中的 NT/LM hash **长度字段**清零（等效空密码），并清除 `F`
//! 结构里的 `ACB_DISABLED` 位（启用账户）。
//!
//! 安全：**操作前强制把 SAM 复制为 `SAM.lrbak`**；只覆盖固定偏移的 4 字节长度
//! 字段，不改 hive 结构、不挪动数据；任何解析失败/越界一律跳过；**成功收尾后删除
//! 备份**（避免在目标系统留下含账户哈希的 SAM 副本），仅出错时保留以便恢复。

use std::path::Path;

use anyhow::Result;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;
use crate::registry::OfflineRegistry;

/// 离线清除目标系统中指定账户的密码（把 SAM 中该用户 V 结构的 NT/LM hash 长度清零）。
///
/// - `target_partition`：目标系统盘，形如 `"C:"`。
/// - `username` 为空时直接返回 `Ok(false)`（不指定用户名不清除，避免误清整盘备份里的所有账户）。
/// - 返回 `Ok(true)` 表示确实清除了某账户的密码；`Ok(false)` 表示未找到匹配账户或本就空密码。
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

/// 离线系统 SAM 中的一个本地账户（只读枚举用）。
#[derive(Debug, Clone)]
pub struct SamAccount {
    /// 账户名（如 Administrator）。
    pub username: String,
    /// 账户 RID（8 位十六进制，如 000001F4）。
    pub rid: String,
    /// 是否被禁用（F 结构的 ACB_DISABLED 位）。
    pub disabled: bool,
}

/// 只读列出目标系统 SAM 中的本地账户（**不修改** SAM，不做备份）。
///
/// - `target_partition`：目标系统盘，形如 `"C:"`。
/// - 返回该系统下可解析出用户名的本地账户列表。
pub fn list_accounts(target_partition: &str) -> Result<Vec<SamAccount>> {
    let sam_hive = format!("{}\\Windows\\System32\\config\\SAM", target_partition);
    if !Path::new(&sam_hive).exists() {
        anyhow::bail!("目标 SAM 配置单元不存在: {}", sam_hive);
    }

    // 只读枚举使用独立的挂载名，避免与清除流程（LR_SAM）冲突。
    OfflineRegistry::load_hive("LR_SAM_RO", &sam_hive)
        .map_err(|e| anyhow::anyhow!("加载 SAM 配置单元失败: {}", e))?;

    let result = (|| -> Result<Vec<SamAccount>> {
        let users_key = "HKLM\\LR_SAM_RO\\SAM\\Domains\\Account\\Users";
        let rids = list_user_rids(users_key)?;
        let mut accounts = Vec::new();
        for rid in rids {
            let user_key = format!("{}\\{}", users_key, rid);
            let v = match reg_read_binary(&user_key, "V") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let name = match parse_v_username(&v) {
                Some(n) if !n.is_empty() => n,
                _ => continue,
            };
            // 读取 F 结构判断账户是否被禁用（偏移 0x38 处 USHORT 标志位）。
            let disabled = reg_read_binary(&user_key, "F")
                .ok()
                .and_then(|f| f.get(0x38..0x3a).map(|s| u16::from_le_bytes([s[0], s[1]])))
                .map(|flags| flags & 0x0001 != 0)
                .unwrap_or(false);
            accounts.push(SamAccount { username: name, rid, disabled });
        }
        Ok(accounts)
    })();

    let _ = OfflineRegistry::unload_hive("LR_SAM_RO");
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
        .args(["add", key, "/v", value, "/t", "REG_BINARY", "/d", &hex, "/f"])
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

    /// 合成一个最小可解析的 SAM "V" 结构。
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

    fn build_f(flags: u16) -> Vec<u8> {
        let mut f = vec![0u8; 0x40];
        f[0x38..0x3a].copy_from_slice(&flags.to_le_bytes());
        f
    }

    #[test]
    fn hex_to_bytes_works() {
        assert_eq!(hex_to_bytes("dEadBeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(hex_to_bytes("de ad\tbe ef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert!(hex_to_bytes("abc").is_err());
        assert_eq!(hex_to_bytes("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn read_u32_le_bounds() {
        assert_eq!(read_u32_le(&[1, 0, 0, 0], 0), Some(1));
        assert_eq!(read_u32_le(&[0xff, 0xff, 0xff, 0xff], 0), Some(0xffff_ffff));
        assert_eq!(read_u32_le(&[1, 2, 3], 0), None);
    }

    #[test]
    fn parse_v_username_basic_and_offset() {
        assert_eq!(
            parse_v_username(&build_v("Administrator", 0, 16, 16)).as_deref(),
            Some("Administrator")
        );
        assert_eq!(parse_v_username(&build_v("用户A", 8, 16, 16)).as_deref(), Some("用户A"));
    }

    #[test]
    fn parse_v_username_edge_cases() {
        assert_eq!(parse_v_username(&[0u8; 0x80]), None);
        assert_eq!(parse_v_username(&build_v("", 0, 0, 0)), None);
        let mut v = build_v("X", 0, 0, 0);
        v[0x10..0x14].copy_from_slice(&9999u32.to_le_bytes());
        assert_eq!(parse_v_username(&v), None);
    }

    #[test]
    fn blank_v_password_zeroes_hash_lengths() {
        let mut v = build_v("u", 0, 16, 16);
        assert!(blank_v_password(&mut v));
        assert_eq!(read_u32_le(&v, 0xa0), Some(0));
        assert_eq!(read_u32_le(&v, 0xac), Some(0));
        assert!(!blank_v_password(&mut v));
    }

    #[test]
    fn blank_v_password_noop_cases() {
        let mut v = build_v("u", 0, 0, 0);
        assert!(!blank_v_password(&mut v));
        assert!(!blank_v_password(&mut vec![0u8; 0x80]));
    }

    #[test]
    fn enable_account_f_clears_disabled_bit() {
        let nf = enable_account_f(&build_f(0x0211)).expect("禁用账户应被改动");
        assert_eq!(u16::from_le_bytes([nf[0x38], nf[0x39]]), 0x0210);
        assert!(enable_account_f(&build_f(0x0210)).is_none());
        assert!(enable_account_f(&[0u8; 0x10]).is_none());
    }
}
