//! 匿名安装量统计客户端（规则见 [`lr_core::install_stats`]，与错误报告共用“关于”页开关）。
//!
//! 开始安装时生成本次安装的编号和令牌并发送 `start`；安装在正常端失败或被取消时发送
//! `failed`；安装成功的确认由新系统首次登录后发送（凭据随安装配置交给 PE / 直接安装流程，
//! 暂存到新系统的 `LetRecovery_Scripts`，读取后立即删除）。所有网络请求都在后台线程，
//! 失败只写日志，绝不影响安装结果。

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use lr_core::install_stats::{InstallTicket, OutcomeReport};

const ENDPOINT: &str = lr_core::branding::INSTALL_STATS_URL;

static CURRENT: Mutex<Option<InstallTicket>> = Mutex::new(None);

fn current() -> std::sync::MutexGuard<'static, Option<InstallTicket>> {
    CURRENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 用户开始一次安装。`mode`：`direct` 或 `via_pe`。关闭统计时不生成编号，也不会写进新系统。
pub fn begin_install(mode: &'static str, running_in_pe: bool, enabled: bool) {
    let ticket = enabled.then(InstallTicket::generate).flatten();
    *current() = ticket.clone();
    let Some(ticket) = ticket else {
        return;
    };
    let environment = if running_in_pe { "pe" } else { "windows" };
    std::thread::spawn(move || match send_start(mode, environment, &ticket) {
        Ok(true) => log::info!("[INSTALL STATS] 已登记本次安装（{mode}）"),
        Ok(false) => log::info!("[INSTALL STATS] 服务器已收到，但本次没有计入（重复或超出限额）"),
        Err(error) => log::warn!("[INSTALL STATS] 安装统计发送失败（不影响安装）: {error:#}"),
    });
}

/// 写入 PE 配置或首次登录脚本时使用的本次安装凭据。
pub fn current_ticket() -> Option<InstallTicket> {
    current().clone()
}

/// 正常端的工作已经交接完成（直接安装完成或 PE 已准备好）；成功与否由新系统确认。
pub fn finish_handoff() {
    current().take();
}

/// 安装失败或取消：`reason` 为 `error` 或 `cancelled`。
pub fn report_failed(reason: &'static str) {
    let Some(ticket) = current().take() else {
        return;
    };
    std::thread::spawn(move || {
        let body = match serde_json::to_vec(&OutcomeReport::failed(&ticket, reason)) {
            Ok(body) => body,
            Err(error) => return log::warn!("[INSTALL STATS] 无法生成失败上报: {error}"),
        };
        match with_fallback(|client| post(client, &body)) {
            Ok(_) => log::info!("[INSTALL STATS] 已上报本次安装未完成（{reason}）"),
            Err(error) => {
                log::warn!("[INSTALL STATS] 失败上报未送达（服务器会在 1 天后按失败计）: {error:#}")
            }
        }
    });
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

/// 与在线目录相同：系统 TLS/代理失败时用内置 Rustls 直连重试一次。
fn with_fallback<T>(request: impl Fn(&reqwest::blocking::Client) -> Result<T>) -> Result<T> {
    match request(&client(false)?) {
        Ok(value) => Ok(value),
        Err(native) => request(&client(true)?).map_err(|direct| {
            anyhow::anyhow!("system TLS/proxy error: {native:#}; direct rustls error: {direct:#}")
        }),
    }
}

fn post(client: &reqwest::blocking::Client, body: &[u8]) -> Result<bool> {
    let response = client
        .post(ENDPOINT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .context("report request failed")?;
    anyhow::ensure!(
        response.status().is_success(),
        "report returned {}",
        response.status()
    );
    let value: serde_json::Value = response.json().context("invalid report response")?;
    Ok(value
        .get("counted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

fn send_start(mode: &str, environment: &str, ticket: &InstallTicket) -> Result<bool> {
    let challenge: lr_core::install_stats::Challenge = with_fallback(|client| {
        let response = client
            .get(format!("{ENDPOINT}?challenge=1"))
            .send()
            .context("challenge request failed")?;
        anyhow::ensure!(
            response.status().is_success(),
            "challenge returned {}",
            response.status()
        );
        response.json().context("invalid challenge")
    })?;
    let report = lr_core::install_stats::solve_start(
        &challenge,
        env!("BUILD_VERSION"),
        mode,
        environment,
        &client_id()?,
        ticket,
    )
    .context("proof of work not found")?;
    let body = serde_json::to_vec(&report)?;
    with_fallback(|client| post(client, &body))
}

/// 每份 LetRecovery 程序随机生成一次，保存在日志目录；只用于服务端按客户端限额，与硬件无关。
fn client_id() -> Result<String> {
    let path = crate::utils::logger::LogManager::get_log_dir().join("install-stats-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if existing.len() == 32 && existing.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(existing.to_ascii_lowercase());
        }
    }
    let id = lr_core::install_stats::random_hex(16).context("random client id")?;
    if let Err(error) = std::fs::write(&path, &id) {
        log::warn!("[INSTALL STATS] 无法保存客户端编号: {error}");
    }
    Ok(id)
}
