use encoding_rs::GBK;

/// 将 GBK 编码的字节转换为 UTF-8 字符串
pub fn gbk_to_utf8(bytes: &[u8]) -> String {
    let (cow, _, _) = GBK.decode(bytes);
    cow.into_owned()
}