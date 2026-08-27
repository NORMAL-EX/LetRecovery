//! 离线 SAM 账户操作（两端共享）：清除指定账户密码、启用被禁用账户。
//!
//! 通过共享 Win32 注册表边界挂载离线 SAM 配置单元，按 chntpw 思路把目标账户
//! 在 SAM `V` 结构中的 NT/LM hash **长度字段**清零（等效空密码），并清除 `F`
//! 结构里的 `ACB_DISABLED` 位（启用账户）。
//!
//! 安全：**操作前强制把 SAM 复制为 `SAM.lrbak`**；只覆盖固定偏移的 4 字节长度
//! 字段，不改 hive 结构、不挪动数据；任何解析失败/越界一律跳过；**成功收尾后删除
//! 备份**（避免在目标系统留下含账户哈希的 SAM 副本），仅出错时保留以便恢复。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use crate::registry::{OfflineRegistry, ReadOnlyAppHive, ReadOnlyOfflineHive};

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
        let mut account_found = false;

        for rid in rids {
            let user_key = format!("{}\\{}", users_key, rid);
            let v = reg_read_binary(&user_key, "V").map_err(|error| {
                anyhow::anyhow!("failed to read SAM V data for RID {rid}: {error}")
            })?;
            let name = parse_v_username(&v).ok_or_else(|| {
                anyhow::anyhow!("invalid SAM V data for RID {rid}; password reset stopped")
            })?;
            if !name.eq_ignore_ascii_case(username) {
                continue;
            }
            account_found = true;

            // 清空 NT/LM hash 长度（等效空密码）
            let mut patched = v.clone();
            if blank_v_password(&mut patched) {
                reg_write_binary(&user_key, "V", &patched)?;
                log::info!("[SAM] 已清除账户 [{}] (RID {}) 的密码", name, rid);
            } else {
                log::info!("[SAM] 账户 [{}] 已是空密码，无需清除", name);
            }

            // 顺带启用被禁用的账户（清除 F 结构中的 ACB_DISABLED 位）
            let f = reg_read_binary(&user_key, "F").map_err(|error| {
                anyhow::anyhow!("failed to read SAM F data for {name}: {error}")
            })?;
            if let Some(new_f) = enable_account_f(&f) {
                reg_write_binary(&user_key, "F", &new_f).map_err(|error| {
                    anyhow::anyhow!("failed to enable SAM account {name}: {error}")
                })?;
                log::info!("[SAM] 已启用账户 [{}]", name);
            }
            break;
        }
        Ok(account_found)
    })();

    let unload_result = OfflineRegistry::unload_hive("LR_SAM")
        .map_err(|error| anyhow::anyhow!("failed to unload SAM hive: {error}"));
    let result = match (result, unload_result) {
        (Err(operation), Err(unload)) => Err(anyhow::anyhow!(
            "SAM operation failed: {operation}; additionally, {unload}"
        )),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(_), Err(unload)) => Err(unload),
        (Ok(found), Ok(())) => Ok(found),
    };

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

fn sam_account_views_match(indexed_names: &[String], accounts: &[SamAccount]) -> bool {
    let mut indexed_names = indexed_names
        .iter()
        .map(|name| name.to_lowercase())
        .collect::<Vec<_>>();
    let mut record_names = accounts
        .iter()
        .map(|account| account.username.to_lowercase())
        .collect::<Vec<_>>();
    indexed_names.sort_unstable();
    record_names.sort_unstable();
    indexed_names == record_names
}

fn sam_users_key_from_root_subkeys(root_subkeys: &[String]) -> Result<Option<&'static str>> {
    if root_subkeys.is_empty() {
        // A generalized Microsoft install image can carry a valid, loadable but not-yet-
        // initialized SAM hive. Windows Setup creates the account domain during specialize/OOBE.
        return Ok(None);
    }
    if root_subkeys
        .iter()
        .any(|name| name.eq_ignore_ascii_case("SAM"))
    {
        return Ok(Some("SAM\\Domains\\Account\\Users"));
    }
    if root_subkeys
        .iter()
        .any(|name| name.eq_ignore_ascii_case("Domains"))
    {
        return Ok(Some("Domains\\Account\\Users"));
    }
    anyhow::bail!("loaded SAM hive has an unexpected root-key layout")
}

fn enumerate_accounts_from_app_hive(hive: &ReadOnlyAppHive) -> Result<Vec<SamAccount>> {
    let root_subkeys = hive.subkey_names("")?;
    let Some(users_key) = sam_users_key_from_root_subkeys(&root_subkeys)? else {
        return Ok(Vec::new());
    };
    let rids = list_user_rids_from_app_hive(hive, users_key)?;
    let mut accounts = Vec::new();
    for rid in rids {
        let user_key = format!("{users_key}\\{rid}");
        let v = hive
            .query_binary(&user_key, "V")
            .map_err(|error| anyhow::anyhow!("failed to read SAM V data for RID {rid}: {error}"))?;
        let name = parse_v_username(&v).ok_or_else(|| {
            anyhow::anyhow!("invalid SAM V data for RID {rid}; enumeration stopped")
        })?;
        if name.is_empty() {
            anyhow::bail!("SAM account name is empty for RID {rid}");
        }
        let f = hive
            .query_binary(&user_key, "F")
            .map_err(|error| anyhow::anyhow!("failed to read SAM F data for {name}: {error}"))?;
        accounts.push(account_from_records(rid, name, &f)?);
    }
    verify_account_name_index(hive.subkey_names(&format!("{users_key}\\Names"))?, accounts)
}

fn enumerate_accounts_from_offline_hive(hive: &ReadOnlyOfflineHive) -> Result<Vec<SamAccount>> {
    let root_subkeys = hive.subkey_names("")?;
    let Some(users_key) = sam_users_key_from_root_subkeys(&root_subkeys)? else {
        return Ok(Vec::new());
    };
    let rids = hive
        .subkey_names(users_key)?
        .into_iter()
        .filter(|name| name.len() == 8 && name.chars().all(|c| c.is_ascii_hexdigit()))
        .collect::<Vec<_>>();
    let mut accounts = Vec::new();
    for rid in rids {
        let user_key = format!("{users_key}\\{rid}");
        let v = hive
            .query_binary(&user_key, "V")
            .map_err(|error| anyhow::anyhow!("failed to read SAM V data for RID {rid}: {error}"))?;
        let name = parse_v_username(&v).ok_or_else(|| {
            anyhow::anyhow!("invalid SAM V data for RID {rid}; enumeration stopped")
        })?;
        if name.is_empty() {
            anyhow::bail!("SAM account name is empty for RID {rid}");
        }
        let f = hive
            .query_binary(&user_key, "F")
            .map_err(|error| anyhow::anyhow!("failed to read SAM F data for {name}: {error}"))?;
        accounts.push(account_from_records(rid, name, &f)?);
    }
    verify_account_name_index(hive.subkey_names(&format!("{users_key}\\Names"))?, accounts)
}

fn account_from_records(rid: String, name: String, f: &[u8]) -> Result<SamAccount> {
    let flags = f
        .get(0x38..0x3a)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
        .ok_or_else(|| anyhow::anyhow!("invalid SAM F data for {name}"))?;
    Ok(SamAccount {
        username: name,
        rid,
        disabled: flags & 0x0001 != 0,
    })
}

fn verify_account_name_index(
    indexed_names: Vec<String>,
    accounts: Vec<SamAccount>,
) -> Result<Vec<SamAccount>> {
    if !sam_account_views_match(&indexed_names, &accounts) {
        anyhow::bail!(
            "SAM Users\\Names index does not exactly match the parsed RID account records"
        );
    }
    Ok(accounts)
}

fn read_accounts_from_loaded_hive(hive_name: &str) -> Result<Vec<SamAccount>> {
    let root = format!("HKLM\\{hive_name}");
    let root_subkeys = OfflineRegistry::subkey_names(&root)?;
    let Some(users_key) = sam_users_key_from_root_subkeys(&root_subkeys)? else {
        return Ok(Vec::new());
    };
    let users_key = format!("{root}\\{users_key}");
    let rids = list_user_rids(&users_key)?;
    let mut accounts = Vec::new();
    for rid in rids {
        let user_key = format!("{users_key}\\{rid}");
        let v = reg_read_binary(&user_key, "V")
            .map_err(|error| anyhow::anyhow!("failed to read SAM V data for RID {rid}: {error}"))?;
        let name = parse_v_username(&v).ok_or_else(|| {
            anyhow::anyhow!("invalid SAM V data for RID {rid}; enumeration stopped")
        })?;
        if name.is_empty() {
            anyhow::bail!("SAM account name is empty for RID {rid}");
        }
        let f = reg_read_binary(&user_key, "F")
            .map_err(|error| anyhow::anyhow!("failed to read SAM F data for {name}: {error}"))?;
        accounts.push(account_from_records(rid, name, &f)?);
    }
    verify_account_name_index(
        OfflineRegistry::subkey_names(&format!("{users_key}\\Names"))?,
        accounts,
    )
}

fn unique_read_only_hive_name() -> String {
    static NEXT_HIVE: AtomicU64 = AtomicU64::new(0);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "LR_SAM_RO_{}_{}_{}",
        std::process::id(),
        nonce,
        NEXT_HIVE.fetch_add(1, Ordering::Relaxed)
    )
}

fn enumerate_accounts_via_loaded_hive(sam_hive: &str) -> Result<Vec<SamAccount>> {
    let hive_name = unique_read_only_hive_name();
    OfflineRegistry::load_hive(&hive_name, sam_hive)?;
    let operation = read_accounts_from_loaded_hive(&hive_name);
    let unload = OfflineRegistry::unload_hive(&hive_name);
    match (operation, unload) {
        (Ok(accounts), Ok(())) => Ok(accounts),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(unload)) => anyhow::bail!(
            "{operation}; additionally failed to unload fallback SAM hive {hive_name}: {unload}"
        ),
    }
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

    // Prefer RegLoadAppKeyW: it returns a private root handle, needs no backup/restore privilege,
    // and unloads when the last handle closes. Some valid Microsoft system SAM templates are not
    // accepted as application hives (observed ERROR_BADDB/1009 after applying the official
    // 28000.2113 image). Microsoft's redistributable Offline Registry library validates and reads
    // these hives without publishing them under HKLM and therefore without applying live-SAM ACLs.
    // RegLoadKeyW remains the final Win7-compatible system-hive fallback: it requires
    // SeRestorePrivilege plus SeBackupPrivilege and must be paired with RegUnLoadKeyW. Every path
    // is read-only, verifies both SAM account views, and treats an explicit close/unload failure as
    // an error. API contracts:
    // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regloadappkeyw
    // https://learn.microsoft.com/windows-hardware/drivers/devtest/offline-registry-library
    // https://learn.microsoft.com/windows-hardware/drivers/devtest/oropenhive
    // https://learn.microsoft.com/windows-hardware/drivers/devtest/orenumkey
    // https://learn.microsoft.com/windows-hardware/drivers/devtest/orgetvalue
    // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regloadkeyw
    // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regunloadkeyw
    let app_result = match ReadOnlyAppHive::open(Path::new(&sam_hive)) {
        Ok(hive) => enumerate_accounts_from_app_hive(&hive),
        Err(error) => Err(error),
    };
    if let Ok(accounts) = app_result {
        return Ok(accounts);
    }
    let app_error = app_result.unwrap_err();

    let offline_result = match ReadOnlyOfflineHive::open(Path::new(&sam_hive)) {
        Ok(hive) => {
            let operation = enumerate_accounts_from_offline_hive(&hive);
            let close = hive.close();
            match (operation, close) {
                (Ok(accounts), Ok(())) => Ok(accounts),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Err(operation), Err(close)) => anyhow::bail!(
                    "{operation}; additionally failed to close the Offline Registry SAM hive: {close}"
                ),
            }
        }
        Err(error) => Err(error),
    };
    if let Ok(accounts) = offline_result {
        return Ok(accounts);
    }
    let offline_error = offline_result.unwrap_err();

    enumerate_accounts_via_loaded_hive(&sam_hive).map_err(|fallback_error| {
        anyhow::anyhow!(
            "RegLoadAppKeyW SAM inspection failed: {app_error}; Microsoft Offline Registry inspection failed: {offline_error}; RegLoadKeyW fallback also failed: {fallback_error}"
        )
    })
}

fn list_user_rids_from_app_hive(hive: &ReadOnlyAppHive, users_key: &str) -> Result<Vec<String>> {
    hive.subkey_names(users_key).map(|names| {
        names
            .into_iter()
            .filter(|name| name.len() == 8 && name.chars().all(|c| c.is_ascii_hexdigit()))
            .collect()
    })
}

/// 枚举 `Users` 键下的用户 RID 子键（8 位十六进制，如 000001F4）。
fn list_user_rids(users_key: &str) -> Result<Vec<String>> {
    OfflineRegistry::subkey_names(users_key).map(|names| {
        names
            .into_iter()
            .filter(|name| name.len() == 8 && name.chars().all(|c| c.is_ascii_hexdigit()))
            .collect()
    })
}

/// 读取注册表 REG_BINARY 值为字节数组。
fn reg_read_binary(key: &str, value: &str) -> Result<Vec<u8>> {
    OfflineRegistry::query_binary(key, value)
}

/// 写入注册表 REG_BINARY 值。
fn reg_write_binary(key: &str, value: &str, data: &[u8]) -> Result<()> {
    OfflineRegistry::set_binary(key, value, data)
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

    fn account(rid: &str, username: &str) -> SamAccount {
        SamAccount {
            username: username.to_owned(),
            rid: rid.to_owned(),
            disabled: false,
        }
    }

    #[test]
    fn sam_name_index_must_exactly_cover_rid_records() {
        let records = vec![
            account("000001F4", "Administrator"),
            account("000003E8", "defaultuser0"),
        ];
        assert!(sam_account_views_match(
            &["DEFAULTUSER0".to_owned(), "administrator".to_owned()],
            &records
        ));
        assert!(!sam_account_views_match(
            &["Administrator".to_owned(), "ExistingUser".to_owned()],
            &records
        ));
    }

    #[test]
    fn sam_root_layout_distinguishes_empty_template_and_initialized_hives() {
        assert_eq!(sam_users_key_from_root_subkeys(&[]).unwrap(), None);
        assert_eq!(
            sam_users_key_from_root_subkeys(&["SAM".to_owned()]).unwrap(),
            Some("SAM\\Domains\\Account\\Users")
        );
        assert_eq!(
            sam_users_key_from_root_subkeys(&["Domains".to_owned()]).unwrap(),
            Some("Domains\\Account\\Users")
        );
        assert!(sam_users_key_from_root_subkeys(&["Unexpected".to_owned()]).is_err());
    }

    #[test]
    #[ignore = "requires LETRECOVERY_SAM_FIXTURE_ROOT pointing at an extracted offline Windows root"]
    fn reads_accounts_from_an_external_offline_windows_fixture() {
        let root = std::env::var("LETRECOVERY_SAM_FIXTURE_ROOT")
            .expect("LETRECOVERY_SAM_FIXTURE_ROOT must identify the extracted Windows root");
        let sam_path = Path::new(&root).join("Windows\\System32\\config\\SAM");
        let hive = ReadOnlyOfflineHive::open(&sam_path)
            .expect("external SAM must load through Microsoft Offline Registry");
        eprintln!(
            "external SAM root subkeys: {:?}",
            hive.subkey_names("").expect("enumerate external SAM root")
        );
        hive.close().expect("close external offline SAM");
        let accounts = list_accounts(&root).expect("external offline SAM must be readable");
        eprintln!("external offline SAM accounts: {accounts:?}");
        assert!(accounts
            .iter()
            .all(|account| !account.username.trim().is_empty()));
    }

    /// 合成一个最小可解析的 SAM "V" 结构。
    fn build_v(username: &str, uoff: u32, lm_len: u32, nt_len: u32) -> Vec<u8> {
        let uname: Vec<u8> = username
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
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
        assert_eq!(
            parse_v_username(&build_v("用户A", 8, 16, 16)).as_deref(),
            Some("用户A")
        );
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
        assert!(!blank_v_password(&mut [0u8; 0x80]));
    }

    #[test]
    fn enable_account_f_clears_disabled_bit() {
        let nf = enable_account_f(&build_f(0x0211)).expect("禁用账户应被改动");
        assert_eq!(u16::from_le_bytes([nf[0x38], nf[0x39]]), 0x0210);
        assert!(enable_account_f(&build_f(0x0210)).is_none());
        assert!(enable_account_f(&[0u8; 0x10]).is_none());
    }
}
