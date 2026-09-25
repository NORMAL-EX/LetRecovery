use anyhow::{Context, Result};
use base64::Engine;

const FEEDBACK_URL: &str = "https://api.cloud-pe.cn/v1/feedback";

pub fn upload_log(log: &str, stage: &str) -> Result<String> {
    let envelope = lr_core::feedback::envelope(log, env!("CARGO_PKG_VERSION"), &session_id(), stage);
    let body = serde_json::to_vec(&envelope)?;
    let response = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(12)).user_agent(concat!("LetRecovery/", env!("CARGO_PKG_VERSION"))).build()?.post(FEEDBACK_URL).header(reqwest::header::CONTENT_TYPE, "application/json").body(base64::engine::general_purpose::STANDARD.encode(body)).send().context("feedback request failed")?;
    if !response.status().is_success() { anyhow::bail!("feedback API returned {}", response.status()); }
    let value: serde_json::Value = response.json().context("invalid feedback response")?;
    value.get("ticket_id").and_then(|v| v.as_str()).map(str::to_owned).ok_or_else(|| anyhow::anyhow!("feedback response omitted ticket_id"))
}

fn session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!("{:x}-{:x}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())
}
