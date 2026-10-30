//! 匿名安装量统计的共享规则（与服务端 `/v1/install` 一致，protocol 2）。
//!
//! 一次安装有三个事件：
//!   - `start`：开始安装时由正常端发送。需要服务端签名的一次性挑战，并完成 SHA-256 前导零
//!     工作量证明，防止接口被刷；同时登记本次安装的随机编号和令牌的 SHA-256。
//!   - `failed`：正常端在安装失败或被取消时发送，凭令牌结束这次安装。
//!   - `succeeded`：新系统首次登录后，由首次登录脚本在后台等网络可用时发送，凭令牌结束这次安装。
//!
//! 服务端把超过 1 天还没有结束事件的安装记为失败。上报内容只有客户端版本、安装方式
//! （`direct` / `via_pe`）、运行环境（`windows` / `pe`）、每份程序一次的随机客户端编号和每次
//! 安装的随机编号，不包含任何硬件、账户或文件信息。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PROTOCOL: u16 = 2;
/// 新系统 `LetRecovery_Scripts` 中暂存的成功上报凭据，首次登录脚本读取后立即删除。
pub const TICKET_FILE_NAME: &str = "install-report.json";
/// 客户端接受的难度范围；服务端要求更高时按上限计算，避免被异常响应拖住。
pub const MIN_DIFFICULTY: u32 = 12;
pub const MAX_DIFFICULTY: u32 = 24;

#[derive(Debug, Clone, Deserialize)]
pub struct Challenge {
    pub challenge: String,
    pub difficulty: u32,
}

/// 一次安装的编号和令牌。令牌只以 SHA-256 形式登记到服务端，结束事件凭原文证明归属。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallTicket {
    pub install_id: String,
    pub token: String,
}

impl InstallTicket {
    pub fn generate() -> Option<Self> {
        Some(Self {
            install_id: random_hex(16)?,
            token: random_hex(32)?,
        })
    }

    pub fn is_valid(&self) -> bool {
        is_lower_hex(&self.install_id, 32) && is_lower_hex(&self.token, 64)
    }

    pub fn token_hash(&self) -> String {
        hex(&Sha256::digest(self.token.as_bytes()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartReport {
    pub protocol: u16,
    pub event: String,
    pub challenge: String,
    pub client_version: String,
    pub mode: String,
    pub environment: String,
    pub client_id: String,
    pub install_id: String,
    pub token_hash: String,
    pub nonce: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeReport {
    pub protocol: u16,
    pub event: String,
    pub install_id: String,
    pub token: String,
    pub reason: String,
}

impl OutcomeReport {
    pub fn failed(ticket: &InstallTicket, reason: &str) -> Self {
        Self {
            protocol: PROTOCOL,
            event: "failed".into(),
            install_id: ticket.install_id.clone(),
            token: ticket.token.clone(),
            reason: reason.into(),
        }
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in digest {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

fn proof_prefix(report: &StartReport) -> String {
    format!(
        "lrinst2|{}|{}|{}|{}|{}|{}|{}|",
        report.challenge,
        report.client_version,
        report.mode,
        report.environment,
        report.client_id,
        report.install_id,
        report.token_hash
    )
}

fn proof_bits(prefix: &str, nonce: u64) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(nonce.to_string().as_bytes());
    leading_zero_bits(&hasher.finalize())
}

/// 计算开始事件的工作量证明。难度 20 平均约 100 万次哈希，普通电脑一秒以内。
pub fn solve_start(
    challenge: &Challenge,
    client_version: &str,
    mode: &str,
    environment: &str,
    client_id: &str,
    ticket: &InstallTicket,
) -> Option<StartReport> {
    let difficulty = challenge.difficulty.clamp(MIN_DIFFICULTY, MAX_DIFFICULTY);
    let mut report = StartReport {
        protocol: PROTOCOL,
        event: "start".into(),
        challenge: challenge.challenge.clone(),
        client_version: client_version.into(),
        mode: mode.into(),
        environment: environment.into(),
        client_id: client_id.into(),
        install_id: ticket.install_id.clone(),
        token_hash: ticket.token_hash(),
        nonce: 0,
    };
    let prefix = proof_prefix(&report);
    report.nonce = (0..(1u64 << 30)).find(|nonce| proof_bits(&prefix, *nonce) >= difficulty)?;
    Some(report)
}

pub fn verify_start(report: &StartReport, difficulty: u32) -> bool {
    proof_bits(&proof_prefix(report), report.nonce) >= difficulty
}

/// 生成 `bytes` 个字节的随机十六进制字符串（Windows 使用系统首选 CSPRNG）。
#[cfg(windows)]
pub fn random_hex(bytes: usize) -> Option<String> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut buffer = vec![0u8; bytes];
    let status = unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            &mut buffer,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    status.is_ok().then(|| hex(&buffer))
}

#[cfg(not(windows))]
pub fn random_hex(bytes: usize) -> Option<String> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    while out.len() < bytes * 2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_usize(out.len());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out.truncate(bytes * 2);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_reports_verify_and_tampering_fails() {
        let ticket = InstallTicket::generate().unwrap();
        assert!(ticket.is_valid());
        let challenge = Challenge {
            challenge: "1761700000.0011223344556677.abcdef".into(),
            difficulty: 12,
        };
        let report = solve_start(
            &challenge,
            "v2026.10.29",
            "via_pe",
            "windows",
            &"a".repeat(32),
            &ticket,
        )
        .unwrap();
        assert!(verify_start(&report, 12));
        assert_eq!(report.token_hash.len(), 64);
        let mut forged = report.clone();
        forged.token_hash = "0".repeat(64);
        assert!(!verify_start(&forged, 12) || forged.nonce == 0);
    }

    #[test]
    fn leading_zero_bits_counts_partial_bytes() {
        assert_eq!(leading_zero_bits(&[0, 0, 0x10, 0xff]), 19);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
    }
}
