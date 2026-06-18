//! 实验性 BitLocker 密钥透传。
//!
//! 正常系统端在"写 PE 引导"阶段，把各 BitLocker 加密卷的恢复密钥写成一个文本文件，
//! 用 [`crate::wimlib::WimlibManager::add_file_to_image`] 打包进 PE 的 boot.wim；PE 启动后
//! 从 `X:\` 读取该文件，对每个锁定的卷逐一尝试这些恢复密钥来解锁，然后继续部署。
//!
//! 采用极简纯文本格式（每行一个恢复密钥，`#` 开头为注释/标签），刻意不给 lr-core 引入
//! 序列化依赖。恢复密钥本身由卷自校验（错误的密钥解锁会失败），因此 PE 端不需要把密钥与
//! 具体卷精确配对——对每个锁定卷把所有密钥都试一遍即可，简单而稳健。

/// 密钥文件在 WIM 镜像内的目标路径（也是 PE 启动后 `X:\` 下的路径）。
pub const KEYS_WIM_PATH: &str = "\\LR_BitLockerKeys.txt";

/// 密钥文件名（PE 端从 `X:\` 拼接读取）。
pub const KEYS_FILE_NAME: &str = "LR_BitLockerKeys.txt";

/// 把若干 `(标签, 恢复密钥)` 组装成密钥文件文本。
pub fn serialize_keys(entries: &[(String, String)]) -> String {
    let mut s = String::from("# LetRecovery BitLocker passthrough (experimental)\r\n");
    for (label, key) in entries {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        if !label.is_empty() {
            s.push_str("# ");
            s.push_str(label);
            s.push_str("\r\n");
        }
        s.push_str(key);
        s.push_str("\r\n");
    }
    s
}

/// 从密钥文件文本解析出恢复密钥列表（跳过空行与 `#` 注释行，去重保序）。
pub fn parse_keys(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line.to_string();
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_skip_comments() {
        let entries = vec![
            ("C:".to_string(), "111111-222222-333333-444444-555555-666666-777777-888888".to_string()),
            ("D: 数据盘".to_string(), "000000-111111-222222-333333-444444-555555-666666-777777".to_string()),
        ];
        let text = serialize_keys(&entries);
        let keys = parse_keys(&text);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], entries[0].1);
        assert_eq!(keys[1], entries[1].1);
    }

    #[test]
    fn parse_dedup_and_blank() {
        let text = "# header\r\nAAA\r\n\r\n  AAA  \r\n# label\r\nBBB\r\n";
        let keys = parse_keys(text);
        assert_eq!(keys, vec!["AAA".to_string(), "BBB".to_string()]);
    }

    #[test]
    fn empty_keys_skipped() {
        let entries = vec![("X".to_string(), "   ".to_string())];
        let text = serialize_keys(&entries);
        assert!(parse_keys(&text).is_empty());
    }
}
