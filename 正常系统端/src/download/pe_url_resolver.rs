//! PE下载URL解析模块
//! 
//! 用于处理PE下载链接的特殊逻辑：
//! 1. 访问下载URL
//! 2. 如果返回JSON，解析出真正的下载链接和headers
//! 3. 如果302跳转，使用跳转后的链接
//! 4. 否则直接使用原始链接
//!
//! 优化点：
//! - 使用全局HTTP客户端，复用TCP连接
//! - 直接使用GET请求，跳过HEAD请求
//! - 优化超时配置，细分连接超时和读取超时
//! - 支持连接预热

use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

/// 全局HTTP客户端（复用连接）
static GLOBAL_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// 全局阻塞式HTTP客户端
static GLOBAL_BLOCKING_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

/// 获取或创建全局异步HTTP客户端
fn get_global_client() -> &'static reqwest::Client {
    GLOBAL_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 不自动跟随重定向，我们需要手动处理
            .redirect(reqwest::redirect::Policy::none())
            // 连接超时：5秒（建立TCP连接的时间）
            .connect_timeout(Duration::from_secs(5))
            // 总超时：30秒（包含连接和数据传输）
            .timeout(Duration::from_secs(30))
            // 启用连接池
            .pool_max_idle_per_host(5)
            // 空闲连接存活时间
            .pool_idle_timeout(Duration::from_secs(90))
            // 启用TCP keepalive
            .tcp_keepalive(Duration::from_secs(30))
            // 启用TCP nodelay减少延迟
            .tcp_nodelay(true)
            // User-Agent
            .user_agent("LetRecovery/2026.1")
            .build()
            .expect("创建HTTP客户端失败")
    })
}

/// 获取或创建全局阻塞式HTTP客户端
fn get_global_blocking_client() -> &'static reqwest::blocking::Client {
    GLOBAL_BLOCKING_CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(5)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .user_agent("LetRecovery/2026.1")
            .build()
            .expect("创建HTTP客户端失败")
    })
}

/// PE下载URL响应（JSON格式）
#[derive(Debug, Clone, Deserialize)]
pub struct PeDownloadResponse {
    /// 是否成功
    pub success: bool,
    /// 实际下载URL
    pub download_url: String,
    /// 需要的请求头
    #[serde(default)]
    pub headers: Vec<String>,
}

/// PE下载URL解析结果
#[derive(Debug, Clone)]
pub struct PeUrlResolveResult {
    /// 最终的下载URL
    pub download_url: String,
    /// 需要传递给aria2的headers（可能为空）
    pub headers: Vec<String>,
}

/// 预热连接到指定域名
/// 
/// 在用户选择PE之前调用，提前建立TCP连接，减少后续请求延迟
pub async fn warmup_connection(url: &str) {
    let client = get_global_client();
    
    // 解析域名
    if let Ok(parsed_url) = reqwest::Url::parse(url) {
        if let Some(host) = parsed_url.host_str() {
            log::info!("[PE URL] 预热连接到: {}", host);
            
            // 发送一个简单的HEAD请求来建立连接
            // 使用短超时，失败也无所谓
            let warmup_url = format!("{}://{}/", parsed_url.scheme(), host);
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                client.head(&warmup_url).send()
            ).await;
            
            log::info!("[PE URL] 连接预热完成: {}", host);
        }
    }
}

/// 预热连接（同步版本）
pub fn warmup_connection_blocking(url: &str) {
    let client = get_global_blocking_client();
    
    if let Ok(parsed_url) = reqwest::Url::parse(url) {
        if let Some(host) = parsed_url.host_str() {
            log::info!("[PE URL] 预热连接到: {}", host);
            
            let warmup_url = format!("{}://{}/", parsed_url.scheme(), host);
            // 创建一个带短超时的临时客户端
            if let Ok(temp_client) = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
            {
                let _ = temp_client.head(&warmup_url).send();
            }
            
            log::info!("[PE URL] 连接预热完成: {}", host);
        }
    }
    
    // 确保全局客户端被初始化
    let _ = client;
}

/// 解析PE下载URL
/// 
/// 流程优化：
/// 1. 直接发送GET请求（跳过HEAD请求，减少一次RTT）
/// 2. 如果返回JSON且包含download_url字段，使用解析出的URL和headers
/// 3. 如果302跳转，使用跳转后的URL
/// 4. 否则使用原始URL
pub async fn resolve_pe_download_url(url: &str) -> Result<PeUrlResolveResult> {
    let start_time = std::time::Instant::now();
    log::info!("[PE URL] 开始解析PE下载链接: {}", url);
    
    let client = get_global_client();
    
    // 直接发送GET请求，跳过HEAD请求以减少延迟
    // 服务器返回的JSON数据通常很小，直接GET更高效
    let response = client.get(url)
        .send()
        .await
        .context("发送请求失败")?;
    
    let elapsed = start_time.elapsed();
    log::info!("[PE URL] 收到响应，状态: {}，耗时: {:?}", response.status(), elapsed);
    
    // 检查是否为重定向
    if response.status().is_redirection() {
        if let Some(location) = response.headers().get(reqwest::header::LOCATION) {
            let redirect_url = location.to_str().context("解析重定向URL失败")?;
            log::info!("[PE URL] 检测到302重定向，目标URL: {}", redirect_url);
            return Ok(PeUrlResolveResult {
                download_url: redirect_url.to_string(),
                headers: Vec::new(),
            });
        }
    }
    
    // 检查是否成功响应
    if !response.status().is_success() {
        log::warn!("[PE URL] 服务器返回错误状态: {}，使用原始URL", response.status());
        return Ok(PeUrlResolveResult {
            download_url: url.to_string(),
            headers: Vec::new(),
        });
    }
    
    // 检查Content-Type，判断是否为JSON
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    
    let is_json = content_type.contains("application/json") 
        || content_type.contains("text/json");
    
    // 获取响应体
    let body = response.text().await.context("读取响应体失败")?;
    
    let total_elapsed = start_time.elapsed();
    log::info!("[PE URL] 读取响应体完成，总耗时: {:?}", total_elapsed);
    
    // 尝试解析为JSON
    if is_json || body.trim().starts_with('{') {
        log::info!("[PE URL] 检测到JSON响应，尝试解析");
        match serde_json::from_str::<PeDownloadResponse>(&body) {
            Ok(pe_response) => {
                if pe_response.success && !pe_response.download_url.is_empty() {
                    log::info!("[PE URL] 成功解析PE下载响应");
                    log::info!("[PE URL] 实际下载URL: {}", pe_response.download_url);
                    log::info!("[PE URL] Headers数量: {}", pe_response.headers.len());
                    
                    // 记录headers（脱敏日志）
                    for (i, header) in pe_response.headers.iter().enumerate() {
                        let header_type = header.split(':').next().unwrap_or("Unknown");
                        log::info!("[PE URL] Header[{}]: {}: ...", i, header_type);
                    }
                    
                    return Ok(PeUrlResolveResult {
                        download_url: pe_response.download_url,
                        headers: pe_response.headers,
                    });
                } else {
                    log::warn!("[PE URL] JSON响应success为false或download_url为空");
                }
            }
            Err(e) => {
                log::warn!("[PE URL] JSON解析失败: {}，尝试当作普通响应处理", e);
            }
        }
    }
    
    // 默认：使用原始URL
    log::info!("[PE URL] 使用原始下载链接");
    Ok(PeUrlResolveResult {
        download_url: url.to_string(),
        headers: Vec::new(),
    })
}

/// 同步版本的resolve_pe_download_url
pub fn resolve_pe_download_url_blocking(url: &str) -> Result<PeUrlResolveResult> {
    let start_time = std::time::Instant::now();
    log::info!("[PE URL] 开始解析PE下载链接(同步): {}", url);
    
    let client = get_global_blocking_client();
    
    // 直接发送GET请求
    let response = client.get(url)
        .send()
        .context("发送请求失败")?;
    
    let elapsed = start_time.elapsed();
    log::info!("[PE URL] 收到响应，状态: {}，耗时: {:?}", response.status(), elapsed);
    
    // 检查是否为重定向
    if response.status().is_redirection() {
        if let Some(location) = response.headers().get(reqwest::header::LOCATION) {
            let redirect_url = location.to_str().context("解析重定向URL失败")?;
            log::info!("[PE URL] 检测到302重定向，目标URL: {}", redirect_url);
            return Ok(PeUrlResolveResult {
                download_url: redirect_url.to_string(),
                headers: Vec::new(),
            });
        }
    }
    
    // 检查是否成功响应
    if !response.status().is_success() {
        log::warn!("[PE URL] 服务器返回错误状态: {}，使用原始URL", response.status());
        return Ok(PeUrlResolveResult {
            download_url: url.to_string(),
            headers: Vec::new(),
        });
    }
    
    // 检查Content-Type
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    
    let is_json = content_type.contains("application/json") 
        || content_type.contains("text/json");
    
    // 获取响应体
    let body = response.text().context("读取响应体失败")?;
    
    let total_elapsed = start_time.elapsed();
    log::info!("[PE URL] 读取响应体完成，总耗时: {:?}", total_elapsed);
    
    // 尝试解析为JSON
    if is_json || body.trim().starts_with('{') {
        log::info!("[PE URL] 检测到JSON响应，尝试解析");
        match serde_json::from_str::<PeDownloadResponse>(&body) {
            Ok(pe_response) => {
                if pe_response.success && !pe_response.download_url.is_empty() {
                    log::info!("[PE URL] 成功解析PE下载响应");
                    log::info!("[PE URL] 实际下载URL: {}", pe_response.download_url);
                    log::info!("[PE URL] Headers数量: {}", pe_response.headers.len());
                    
                    for (i, header) in pe_response.headers.iter().enumerate() {
                        let header_type = header.split(':').next().unwrap_or("Unknown");
                        log::info!("[PE URL] Header[{}]: {}: ...", i, header_type);
                    }
                    
                    return Ok(PeUrlResolveResult {
                        download_url: pe_response.download_url,
                        headers: pe_response.headers,
                    });
                } else {
                    log::warn!("[PE URL] JSON响应success为false或download_url为空");
                }
            }
            Err(e) => {
                log::warn!("[PE URL] JSON解析失败: {}，尝试当作普通响应处理", e);
            }
        }
    }
    
    log::info!("[PE URL] 使用原始下载链接");
    Ok(PeUrlResolveResult {
        download_url: url.to_string(),
        headers: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_pe_download_response() {
        let json = r#"{
            "success": true,
            "download_url": "https://example.com/file.wim",
            "headers": [
                "Cookie: auth=xxx",
                "User-Agent: Mozilla/5.0"
            ]
        }"#;
        
        let response: PeDownloadResponse = serde_json::from_str(json).unwrap();
        assert!(response.success);
        assert_eq!(response.download_url, "https://example.com/file.wim");
        assert_eq!(response.headers.len(), 2);
    }
    
    #[test]
    fn test_parse_pe_download_response_no_headers() {
        let json = r#"{
            "success": true,
            "download_url": "https://example.com/file.wim"
        }"#;
        
        let response: PeDownloadResponse = serde_json::from_str(json).unwrap();
        assert!(response.success);
        assert_eq!(response.download_url, "https://example.com/file.wim");
        assert!(response.headers.is_empty());
    }
}
