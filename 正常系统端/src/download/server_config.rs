//! 服务器配置模块
//! 从远程服务器获取 PE 和系统镜像配置

use anyhow::{Context, Result};
use serde::Deserialize;

/// 全局服务器地址
pub const SERVER_BASE_URL: &str = "https://letrecovery.cloud-pe.cn/v2/";

/// 服务器配置响应
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfigResponse {
    pub code: i32,
    pub message: String,
    pub data: ServerConfigData,
}

/// 服务器配置数据
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfigData {
    pub pe: String,
    pub dl: String,
    #[serde(default)]
    pub soft: Option<String>,
    /// 小白模式配置路径
    #[serde(default)]
    pub easy: Option<String>,
}

/// 远程配置
#[derive(Debug, Clone, Default)]
pub struct RemoteConfig {
    /// PE 列表内容（从服务器获取）
    pub pe_content: Option<String>,
    /// 系统镜像列表内容（从服务器获取）
    pub dl_content: Option<String>,
    /// 软件列表内容（从服务器获取）
    pub soft_content: Option<String>,
    /// 小白模式配置内容（从服务器获取）
    pub easy_content: Option<String>,
    /// 是否加载成功
    pub loaded: bool,
    /// 错误信息
    pub error: Option<String>,
}

impl RemoteConfig {
    /// 从服务器加载配置
    /// 
    /// 流程：
    /// 1. 请求服务器获取配置文件 URL
    /// 2. 根据返回的 URL 获取 PE 和系统镜像列表的内容
    /// 3. 支持完整 URL 和相对路径两种格式
    pub fn load_from_server() -> Self {
        let mut config = RemoteConfig::default();
        
        // 尝试加载配置
        match Self::fetch_config() {
            Ok((pe_content, dl_content, soft_content, easy_content)) => {
                config.pe_content = pe_content;
                config.dl_content = dl_content;
                config.soft_content = soft_content;
                config.easy_content = easy_content;
                config.loaded = true;
                log::info!("远程配置加载成功");
            }
            Err(e) => {
                config.error = Some(e.to_string());
                config.loaded = false;
                log::warn!("远程配置加载失败: {}", e);
            }
        }
        
        config
    }
    
    /// 获取服务器配置
    fn fetch_config() -> Result<(Option<String>, Option<String>, Option<String>, Option<String>)> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context("创建 HTTP 客户端失败")?;
        
        // 请求服务器配置
        let config_url = SERVER_BASE_URL;
        log::info!("请求服务器配置: {}", config_url);
        
        let response = client
            .get(config_url)
            .send()
            .context("请求服务器配置失败")?;
        
        if !response.status().is_success() {
            anyhow::bail!("服务器返回错误状态码: {}", response.status());
        }
        
        let config_response: ServerConfigResponse = response
            .json()
            .context("解析服务器响应失败")?;
        
        if config_response.code != 200 {
            anyhow::bail!("服务器返回错误: {}", config_response.message);
        }
        
        let data = config_response.data;
        
        // 构建 PE 和 DL 的完整 URL
        let pe_url = Self::resolve_url(&data.pe);
        let dl_url = Self::resolve_url(&data.dl);
        let soft_url = data.soft.as_ref().map(|s| Self::resolve_url(s));
        let easy_url = data.easy.as_ref().map(|s| Self::resolve_url(s));
        
        log::info!("PE 配置 URL: {}", pe_url);
        log::info!("DL 配置 URL: {}", dl_url);
        if let Some(ref url) = soft_url {
            log::info!("Soft 配置 URL: {}", url);
        }
        if let Some(ref url) = easy_url {
            log::info!("Easy 配置 URL: {}", url);
        }
        
        // 获取 PE 配置内容
        let pe_content = Self::fetch_text_content(&client, &pe_url).ok();
        
        // 获取 DL 配置内容
        let dl_content = Self::fetch_text_content(&client, &dl_url).ok();
        
        // 获取 Soft 配置内容
        let soft_content = soft_url.and_then(|url| Self::fetch_text_content(&client, &url).ok());
        
        // 获取 Easy 配置内容
        let easy_content = easy_url.and_then(|url| Self::fetch_text_content(&client, &url).ok());
        
        Ok((pe_content, dl_content, soft_content, easy_content))
    }
    
    /// 解析 URL，支持完整 URL 和相对路径
    /// 
    /// 如果是相对路径，则拼接服务器基础地址
    /// 如果是完整 URL，则直接使用
    fn resolve_url(path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            // 完整 URL，直接返回
            path.to_string()
        } else {
            // 相对路径，拼接服务器地址
            format!("{}{}", SERVER_BASE_URL, path.trim_start_matches('/'))
        }
    }
    
    /// 获取文本内容
    fn fetch_text_content(client: &reqwest::blocking::Client, url: &str) -> Result<String> {
        let response = client
            .get(url)
            .send()
            .context(format!("请求 {} 失败", url))?;
        
        if !response.status().is_success() {
            anyhow::bail!("请求 {} 返回错误状态码: {}", url, response.status());
        }
        
        let content = response.text().context("读取响应内容失败")?;
        
        Ok(content)
    }
    
    /// 检查 PE 配置是否可用
    pub fn is_pe_available(&self) -> bool {
        self.pe_content.as_ref().map(|c| !c.trim().is_empty()).unwrap_or(false)
    }
    
    /// 检查系统镜像配置是否可用
    pub fn is_dl_available(&self) -> bool {
        self.dl_content.as_ref().map(|c| !c.trim().is_empty()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_resolve_url_relative() {
        assert_eq!(
            RemoteConfig::resolve_url("config/pe"),
            "https://letrecovery.cloud-pe.cn/v2/config/pe"
        );
    }
    
    #[test]
    fn test_resolve_url_absolute() {
        assert_eq!(
            RemoteConfig::resolve_url("https://example.com/config/pe"),
            "https://example.com/config/pe"
        );
    }
}
