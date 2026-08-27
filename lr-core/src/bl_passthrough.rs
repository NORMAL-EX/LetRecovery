//! Session-scoped BitLocker recovery-password bundle for authenticated PE maintenance.
//!
//! The bytes are allowed only as a `ProtectedBitLockerSecret` artifact in the private boot WIM.
//! The public config and manifest carry only length/hash bindings. A wrong recovery password is
//! rejected by BitLocker itself, so PE can try the small deduplicated set against each currently
//! locked volume without treating mutable drive letters or disk inventory as cross-boot identity.

use zeroize::Zeroizing;

const MAGIC: &str = "LRBL1";
pub const MAX_KEYS: usize = 26;
pub const MAX_BUNDLE_BYTES: u64 = 32 * 1024;

/// 密钥文件在 WIM 镜像内的目标路径（也是 PE 启动后 `X:\` 下的路径）。
pub const KEYS_WIM_PATH: &str = "\\LR_BitLockerKeys.txt";

/// 密钥文件名（PE 端从 `X:\` 拼接读取）。
pub const KEYS_FILE_NAME: &str = "LR_BitLockerKeys.txt";

/// Serialize canonical 48-digit recovery passwords without volume labels or other inventory.
pub fn serialize_keys(entries: &[String]) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut keys = Zeroizing::new(Vec::<String>::new());
    for entry in entries {
        let key = crate::fveapi::format_recovery_key(entry)?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.is_empty() || keys.len() > MAX_KEYS {
        return Err("BitLocker recovery-password count is outside the supported range".into());
    }
    let mut text = format!("{MAGIC}\r\nCount={}\r\n", keys.len());
    for key in keys.iter() {
        text.push_str("Key=");
        text.push_str(key);
        text.push_str("\r\n");
    }
    if text.len() as u64 > MAX_BUNDLE_BYTES {
        return Err("BitLocker recovery-password bundle exceeds its byte limit".into());
    }
    Ok(Zeroizing::new(text.into_bytes()))
}

/// Parse only the exact canonical form emitted by [`serialize_keys`].
pub fn parse_keys(content: &[u8]) -> Result<Zeroizing<Vec<String>>, String> {
    if content.is_empty() || content.len() as u64 > MAX_BUNDLE_BYTES {
        return Err("BitLocker recovery-password bundle length is outside its limit".into());
    }
    let text = std::str::from_utf8(content)
        .map_err(|_| "BitLocker recovery-password bundle is not UTF-8".to_string())?;
    if !text.ends_with("\r\n") || text.replace("\r\n", "").contains(['\r', '\n']) {
        return Err("BitLocker recovery-password bundle has invalid line endings".into());
    }
    let mut lines = text.split("\r\n");
    if lines.next() != Some(MAGIC) {
        return Err("unsupported BitLocker recovery-password bundle".into());
    }
    let count = lines
        .next()
        .and_then(|line| line.strip_prefix("Count="))
        .ok_or_else(|| "BitLocker recovery-password bundle has no count".to_string())?
        .parse::<usize>()
        .map_err(|_| "BitLocker recovery-password count is invalid".to_string())?;
    if count == 0 || count > MAX_KEYS {
        return Err("BitLocker recovery-password count is outside the supported range".into());
    }
    let mut keys = Zeroizing::new(Vec::with_capacity(count));
    for _ in 0..count {
        let raw = lines
            .next()
            .and_then(|line| line.strip_prefix("Key="))
            .ok_or_else(|| "BitLocker recovery-password entry is missing".to_string())?;
        let key = crate::fveapi::format_recovery_key(raw)?;
        if keys.contains(&key) {
            return Err("BitLocker recovery-password bundle contains a duplicate".into());
        }
        keys.push(key);
    }
    if lines.any(|line| !line.is_empty()) {
        return Err("BitLocker recovery-password bundle has trailing fields".into());
    }
    if serialize_keys(&keys)?[..] != content[..] {
        return Err("BitLocker recovery-password bundle is not canonical".into());
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_skip_comments() {
        let entries = vec![
            "111111-222222-333333-444444-555555-666666-777777-888888".to_string(),
            "000000-111111-222222-333333-444444-555555-666666-777777".to_string(),
        ];
        let text = serialize_keys(&entries).unwrap();
        let keys = parse_keys(&text).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], entries[0]);
        assert_eq!(keys[1], entries[1]);
    }

    #[test]
    fn parse_dedup_and_blank() {
        let duplicate = b"LRBL1\r\nCount=2\r\nKey=111111-222222-333333-444444-555555-666666-777777-888888\r\nKey=111111-222222-333333-444444-555555-666666-777777-888888\r\n";
        assert!(parse_keys(duplicate).is_err());
    }

    #[test]
    fn empty_keys_skipped() {
        assert!(serialize_keys(&[]).is_err());
        assert!(parse_keys(b"LRBL1\nCount=1\n").is_err());
    }
}
