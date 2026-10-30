//! 自动错误报告客户端。只上传 [`lr_core::feedback::build_report`] 提取的摘要，同一问题
//! 24 小时内只发送一次；网络请求在后台线程执行，界面最多等待调用方给定的时间。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FEEDBACK_URL: &str = lr_core::branding::FEEDBACK_URL;
const RESEND_AFTER_SECS: u64 = 24 * 60 * 60;
const MAX_REMEMBERED: usize = 64;

/// 读取日志、生成摘要并上传。最多等待 `wait`；超时后上传在后台继续，结果写入日志。
/// 返回 `None` 表示还没有结果。
pub fn send_error_report(log_path: &Path, stage: &str, wait: Duration) -> Option<Result<String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let log_path = log_path.to_path_buf();
    let stage = stage.to_owned();
    std::thread::spawn(move || {
        let result = upload_from_log_file(&log_path, &stage);
        match &result {
            Ok(ticket) => log::info!("[FEEDBACK] 错误报告已发送: {ticket}"),
            Err(error) => log::warn!("[FEEDBACK] 错误报告发送失败: {error:#}"),
        }
        let _ = sender.send(result);
    });
    receiver.recv_timeout(wait).ok()
}

fn upload_from_log_file(log_path: &Path, stage: &str) -> Result<String> {
    let bytes = std::fs::read(log_path).with_context(|| format!("read {}", log_path.display()))?;
    upload_log(&String::from_utf8_lossy(&bytes), stage)
}

pub fn upload_log(log: &str, stage: &str) -> Result<String> {
    let report = lr_core::feedback::build_report(log, env!("BUILD_VERSION"), stage)
        .context("日志中没有可以报告的错误")?;
    if let Some(ticket) = recently_sent(&report.fingerprint) {
        log::info!("[FEEDBACK] 同一问题 24 小时内已经报告过，沿用编号 {ticket}");
        return Ok(ticket);
    }
    let body = serde_json::to_vec(&report)?;
    let ticket = post_report(&body)?;
    remember_sent(&report.fingerprint, &ticket);
    Ok(ticket)
}

fn client(direct: bool) -> Result<reqwest::blocking::Client> {
    let builder = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("LetRecovery/", env!("BUILD_VERSION")));
    let builder = if direct {
        builder.use_rustls_tls().no_proxy()
    } else {
        builder
    };
    Ok(builder.build()?)
}

fn post_report(body: &[u8]) -> Result<String> {
    match send_once(&client(false)?, body) {
        Ok(ticket) => Ok(ticket),
        // 与在线目录相同：系统 TLS/代理异常（例如握手阶段被中断）时，改用内置 Rustls 直连重试一次。
        Err(native_error) => send_once(&client(true)?, body).map_err(|direct_error| {
            anyhow::anyhow!(
                "system TLS/proxy error: {native_error:#}; direct rustls error: {direct_error:#}"
            )
        }),
    }
}

fn send_once(client: &reqwest::blocking::Client, body: &[u8]) -> Result<String> {
    let response = client
        .post(FEEDBACK_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .context("feedback request failed")?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("feedback API returned {status}");
    }
    let value: serde_json::Value = response.json().context("invalid feedback response")?;
    value
        .get("ticket_id")
        .and_then(|value| value.as_str())
        .filter(|ticket| {
            !ticket.is_empty()
                && ticket.len() <= 64
                && ticket
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("feedback response omitted ticket_id"))
}

fn sent_index_path() -> PathBuf {
    crate::utils::logger::LogManager::get_log_dir().join("feedback-sent.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn load_sent() -> HashMap<String, (String, u64)> {
    std::fs::read(sent_index_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn recently_sent(fingerprint: &str) -> Option<String> {
    let (ticket, sent_at) = load_sent().remove(fingerprint)?;
    (now_secs().saturating_sub(sent_at) < RESEND_AFTER_SECS).then_some(ticket)
}

fn remember_sent(fingerprint: &str, ticket: &str) {
    let now = now_secs();
    let mut entries: Vec<(String, (String, u64))> = load_sent()
        .into_iter()
        .filter(|(_, (_, sent_at))| now.saturating_sub(*sent_at) < RESEND_AFTER_SECS)
        .collect();
    entries.retain(|(key, _)| key != fingerprint);
    entries.push((fingerprint.to_owned(), (ticket.to_owned(), now)));
    entries.sort_by_key(|(_, (_, sent_at))| std::cmp::Reverse(*sent_at));
    entries.truncate(MAX_REMEMBERED);
    let index: HashMap<_, _> = entries.into_iter().collect();
    match serde_json::to_vec(&index) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(sent_index_path(), bytes) {
                log::warn!("[FEEDBACK] 无法记录已发送的错误报告: {error}");
            }
        }
        Err(error) => log::warn!("[FEEDBACK] 无法序列化已发送记录: {error}"),
    }
}
