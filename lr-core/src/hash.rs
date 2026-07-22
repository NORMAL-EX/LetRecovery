//! 文件哈希（SHA-256 与兼容用 MD5）生成与校验（两端共享，纯逻辑）。
//!
//! 用于给 WIM/ESD/ISO/GHO 等镜像生成校验值、核对下载完整性，
//! 与 wimlib 的内部完整性校验互补（后者只覆盖 WIM 系列）。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use md5::Md5;
use sha2::{Digest, Sha256};

/// 计算字节数据的 SHA-256，返回小写十六进制字符串。
pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    to_hex(&hasher.finalize())
}

/// 从任意 reader 流式计算 SHA-256；每读取一块回调「累计已读字节数」（用于进度）。
pub fn sha256_reader<R: Read>(
    mut reader: R,
    mut on_progress: impl FnMut(u64),
) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
        on_progress(total);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Copy all bytes from `reader` to `writer` while calculating the SHA-256 of
/// the exact byte stream written to the destination.
///
/// The digest lets staging code prove that a subsequently re-read destination
/// still contains the bytes supplied by the source handle, without reopening
/// the source through a path that may have changed meanwhile.
pub fn copy_and_sha256<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    mut on_progress: impl FnMut(u64) -> std::io::Result<()>,
) -> std::io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        total = total.saturating_add(count as u64);
        on_progress(total)?;
    }
    writer.flush()?;
    Ok((total, to_hex(&hasher.finalize())))
}

/// 计算文件的 SHA-256（流式，回调累计已读字节数）。
pub fn sha256_file(
    path: impl AsRef<Path>,
    on_progress: impl FnMut(u64),
) -> std::io::Result<String> {
    let file = File::open(path)?;
    sha256_reader(file, on_progress)
}

/// Calculate an MD5 digest from a reader.
///
/// MD5 is retained only for compatibility with existing PE metadata. New
/// metadata should publish SHA-256 instead.
pub fn md5_reader<R: Read>(
    mut reader: R,
    mut on_progress: impl FnMut(u64),
) -> std::io::Result<String> {
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
        on_progress(total);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Calculate an MD5 digest for a file.
pub fn md5_file(path: impl AsRef<Path>, on_progress: impl FnMut(u64)) -> std::io::Result<String> {
    let file = File::open(path)?;
    md5_reader(file, on_progress)
}

/// Calculate an MD5 digest for in-memory data.
pub fn md5_bytes(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    to_hex(&hasher.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 规范化用户输入/对照的哈希：去除所有空白并转小写。
pub fn normalize_hash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// 比对计算出的哈希与期望值（忽略大小写与空白）。期望为空时一律视为不匹配。
pub fn hash_matches(computed: &str, expected: &str) -> bool {
    !expected.trim().is_empty() && normalize_hash(computed) == normalize_hash(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digests() {
        // 标准测试向量
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(md5_bytes(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn reader_matches_bytes_and_reports_progress() {
        let data = vec![0x61u8; 3_000_000]; // 3MB，跨多个 1MiB 块
        let mut last = 0u64;
        let h = sha256_reader(&data[..], |t| last = t).unwrap();
        assert_eq!(h, sha256_bytes(&data));
        assert_eq!(last, data.len() as u64); // 进度最终到达总字节数
    }

    #[test]
    fn normalize_and_match() {
        assert_eq!(normalize_hash(" BA78 16bf\n"), "ba7816bf");
        assert!(hash_matches("ABCDEF", "ab cd ef")); // 忽略大小写/空白
        assert!(!hash_matches("abcdef", "abcde0"));
        assert!(!hash_matches("abcdef", "   ")); // 期望为空 → 不匹配
    }
}
