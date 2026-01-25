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
