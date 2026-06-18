//! GBK <-> UTF-8 编码转换（两端共享）。

use encoding_rs::GBK;

/// 将 GBK 编码的字节转换为 UTF-8 字符串
pub fn gbk_to_utf8(bytes: &[u8]) -> String {
    let (cow, _, _) = GBK.decode(bytes);
    cow.into_owned()
}

/// 将 UTF-8 字符串转换为 GBK 编码的字节
pub fn utf8_to_gbk(s: &str) -> Vec<u8> {
    let (cow, _, _) = GBK.encode(s);
    cow.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_passthrough() {
        assert_eq!(gbk_to_utf8(b"hello reg.exe"), "hello reg.exe");
        assert_eq!(utf8_to_gbk("hello"), b"hello");
    }

    #[test]
    fn chinese_known_bytes() {
        // “你好” 的 GBK 编码 = C4 E3 BA C3
        assert_eq!(gbk_to_utf8(&[0xC4, 0xE3, 0xBA, 0xC3]), "你好");
        assert_eq!(utf8_to_gbk("你好"), vec![0xC4, 0xE3, 0xBA, 0xC3]);
    }

    #[test]
    fn roundtrip_utf8_gbk_utf8() {
        for s in ["系统备份", "加载离线注册表配置单元失败", "Administrator 管理员"] {
            assert_eq!(gbk_to_utf8(&utf8_to_gbk(s)), s);
        }
    }
}
