//! 服务器配置模块
//! 从远程服务器获取 PE 和系统镜像配置

use crate::{
    download::config::{
        EasyModeConfig, GpuDriverList, OnlineGpuDriver, OnlinePE, OnlineSoftware, OnlineSystem,
        SoftwareList,
    },
    tr,
};
use anyhow::{Context, Result};
use lr_core::download_integrity::validate_download_url;
use serde::Deserialize;

/// v3 单文件资源目录。正常情况下只需要一次 HTTP 请求。
pub const SERVER_V3_URL: &str = "https://letrecovery.cloud-pe.cn/v3/index.json";

/// v2 多文件资源目录，仅用于 v3 不可用或格式无效时的兼容回退。
pub const SERVER_BASE_URL: &str = "https://letrecovery.cloud-pe.cn/v2/";

type RemoteConfigContents = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct V3CatalogResponse {
    schema_version: u32,
    data: V3CatalogData,
}

#[derive(Debug, Deserialize)]
struct V3CatalogData {
    pe: Vec<V3PeEntry>,
    system_images: Vec<V3SystemEntry>,
    easy_mode: EasyModeConfig,
    software: Vec<V3SoftwareEntry>,
    gpu_drivers: Vec<V3GpuDriverEntry>,
}

#[derive(Debug, Deserialize)]
struct V3PeEntry {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(flatten)]
    value: OnlinePE,
}

#[derive(Debug, Deserialize)]
struct V3SystemEntry {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(flatten)]
    value: OnlineSystem,
}

#[derive(Debug, Deserialize)]
struct V3SoftwareEntry {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(flatten)]
    value: OnlineSoftware,
}

#[derive(Debug, Deserialize)]
struct V3GpuDriverEntry {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(flatten)]
    value: OnlineGpuDriver,
}

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
    /// GPU驱动配置路径
    #[serde(default)]
    pub gpu: Option<String>,
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
    /// GPU驱动列表内容（从服务器获取）
    pub gpu_content: Option<String>,
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
            Ok((pe_content, dl_content, soft_content, easy_content, gpu_content)) => {
                config.pe_content = pe_content;
                config.dl_content = dl_content;
                config.soft_content = soft_content;
                config.easy_content = easy_content;
                config.gpu_content = gpu_content;
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

    /// 优先获取 v3 单文件目录；失败时回退到现有 v2 多文件目录。
    fn fetch_config() -> Result<RemoteConfigContents> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context(tr!("创建 HTTP 客户端失败"))?;

        match Self::fetch_v3_config(&client) {
            Ok(contents) => {
                log::info!("远程资源目录已通过 v3 单请求加载");
                return Ok(contents);
            }
            Err(error) => {
                log::warn!("v3 资源目录不可用，将回退 v2: {error:#}");
            }
        }

        Self::fetch_v2_config(&client).context(tr!("请求服务器配置失败"))
    }

    fn fetch_v3_config(client: &reqwest::blocking::Client) -> Result<RemoteConfigContents> {
        log::info!("请求 v3 服务器配置: {}", SERVER_V3_URL);
        let response = client
            .get(SERVER_V3_URL)
            .send()
            .context(tr!("请求服务器配置失败"))?;

        if !response.status().is_success() {
            anyhow::bail!("{}", tr!("服务器返回错误状态码: {}", response.status()));
        }

        let catalog: V3CatalogResponse = response.json().context(tr!("解析服务器响应失败"))?;
        Self::v3_catalog_to_contents(catalog)
    }

    fn v3_catalog_to_contents(catalog: V3CatalogResponse) -> Result<RemoteConfigContents> {
        if catalog.schema_version != 3 {
            anyhow::bail!("unsupported v3 schema version: {}", catalog.schema_version);
        }

        let pe_list: Vec<OnlinePE> = catalog
            .data
            .pe
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.value)
            .collect();
        let system_list: Vec<OnlineSystem> = catalog
            .data
            .system_images
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.value)
            .collect();
        if pe_list.is_empty() || system_list.is_empty() {
            anyhow::bail!("v3 catalogue must contain enabled PE and system image entries");
        }

        let software = catalog
            .data
            .software
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.value)
            .collect();
        let gpu_drivers = catalog
            .data
            .gpu_drivers
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.value)
            .collect();

        let pe_content = serde_json::to_string(&pe_list).context("serialize v3 PE catalogue")?;
        let dl_content =
            serde_json::to_string(&system_list).context("serialize v3 system catalogue")?;
        let soft_content = serde_json::to_string(&SoftwareList { software })
            .context("serialize v3 software catalogue")?;
        let easy_content = serde_json::to_string(&catalog.data.easy_mode)
            .context("serialize v3 easy-mode catalogue")?;
        let gpu_content = serde_json::to_string(&GpuDriverList {
            software: gpu_drivers,
        })
        .context("serialize v3 GPU catalogue")?;

        Ok((
            Some(pe_content),
            Some(dl_content),
            Some(soft_content),
            Some(easy_content),
            Some(gpu_content),
        ))
    }

    fn fetch_v2_config(client: &reqwest::blocking::Client) -> Result<RemoteConfigContents> {
        let config_url = SERVER_BASE_URL;
        log::info!("请求 v2 服务器配置: {}", config_url);

        let response = client
            .get(config_url)
            .send()
            .context(tr!("请求服务器配置失败"))?;

        if !response.status().is_success() {
            anyhow::bail!("{}", tr!("服务器返回错误状态码: {}", response.status()));
        }

        let config_response: ServerConfigResponse =
            response.json().context(tr!("解析服务器响应失败"))?;

        if config_response.code != 200 {
            anyhow::bail!("{}", tr!("服务器返回错误: {}", config_response.message));
        }

        let data = config_response.data;

        // 构建 PE 和 DL 的完整 URL
        let pe_url = Self::resolve_catalogue_url(&data.pe)?;
        let dl_url = Self::resolve_catalogue_url(&data.dl)?;
        let soft_url = data
            .soft
            .as_deref()
            .map(Self::resolve_catalogue_url)
            .transpose()?;
        let easy_url = data
            .easy
            .as_deref()
            .map(Self::resolve_catalogue_url)
            .transpose()?;
        let gpu_url = data
            .gpu
            .as_deref()
            .map(Self::resolve_catalogue_url)
            .transpose()?;

        log::info!("PE 配置 URL: {}", pe_url);
        log::info!("DL 配置 URL: {}", dl_url);
        if let Some(ref url) = soft_url {
            log::info!("Soft 配置 URL: {}", url);
        }
        if let Some(ref url) = easy_url {
            log::info!("Easy 配置 URL: {}", url);
        }
        if let Some(ref url) = gpu_url {
            log::info!("GPU 配置 URL: {}", url);
        }

        // 获取 PE 配置内容
        let pe_content =
            Some(Self::fetch_text_content(client, &pe_url).context(tr!("加载 PE 资源目录失败"))?);

        // 获取 DL 配置内容
        let dl_content =
            Some(Self::fetch_text_content(client, &dl_url).context(tr!("加载系统镜像目录失败"))?);

        // 获取 Soft 配置内容
        let soft_content = soft_url
            .map(|url| Self::fetch_text_content(client, &url).context(tr!("加载软件目录失败")))
            .transpose()?;

        // 获取 Easy 配置内容
        let easy_content = easy_url
            .map(|url| Self::fetch_text_content(client, &url).context(tr!("加载小白模式目录失败")))
            .transpose()?;

        // 获取 GPU 配置内容
        let gpu_content = gpu_url
            .map(|url| Self::fetch_text_content(client, &url).context(tr!("加载显卡驱动目录失败")))
            .transpose()?;

        Ok((
            pe_content,
            dl_content,
            soft_content,
            easy_content,
            gpu_content,
        ))
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

    /// The fixed endpoint is the trust boundary for legacy payload URLs.  Its
    /// child catalogues must therefore remain HTTPS; otherwise an HTTP
    /// catalogue could silently replace the exact payload URL later selected
    /// by the controller.
    fn resolve_catalogue_url(path: &str) -> Result<String> {
        let url = Self::resolve_url(path);
        validate_download_url(&url, false)
            .map(|validated| validated.into_string())
            .map_err(|error| anyhow::anyhow!(tr!("下载地址不安全或无效: {}", error)))
    }

    /// 获取文本内容
    fn fetch_text_content(client: &reqwest::blocking::Client, url: &str) -> Result<String> {
        let response = client.get(url).send().context(tr!("请求 {} 失败", url))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "{}",
                tr!("请求 {} 返回错误状态码: {}", url, response.status())
            );
        }

        let content = response.text().context(tr!("读取响应内容失败"))?;

        Ok(content)
    }

    /// 检查 PE 配置是否可用
    pub fn is_pe_available(&self) -> bool {
        self.pe_content
            .as_ref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
    }

    /// 检查系统镜像配置是否可用
    pub fn is_dl_available(&self) -> bool {
        self.dl_content
            .as_ref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::config::ConfigManager;

    const V3_FIXTURE: &str = r#"
    {
      "schema_version": 3,
      "data": {
        "pe": [
          {
            "download_url": "https://example.com/LetRecovery_PE.wim",
            "display_name": "LetRecovery PE",
            "filename": "LetRecovery_PE.wim",
            "md5": "900150983CD24FB0D6963F7D28E17F72",
            "sha256": "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "enabled": true
          }
        ],
        "system_images": [
          {
            "download_url": "https://example.com/windows-11.esd",
            "display_name": "Windows 11",
            "is_win11": true,
            "enabled": true
          },
          {
            "download_url": "https://example.com/disabled.esd",
            "display_name": "Disabled",
            "is_win11": false,
            "enabled": false
          }
        ],
        "easy_mode": {
          "system": [
            {
              "Windows 11": {
                "os_logo": "LOGO_WINDOWS11",
                "os_download": "https://example.com/windows-11.esd",
                "volume": [{"number": 1, "name": "Professional"}]
              }
            }
          ]
        },
        "software": [
          {
            "name": "Tool",
            "description": "Description",
            "update_date": "2026-07-15",
            "file_size": "1 MB",
            "download_url": "https://example.com/tool.exe",
            "filename": "tool.exe",
            "enabled": true
          }
        ],
        "gpu_drivers": [
          {
            "name": "Driver",
            "description": "Description",
            "update_date": "2026-07-15",
            "file_size": "1 MB",
            "download_url": "https://example.com/driver.exe",
            "filename": "driver.exe",
            "enabled": true
          }
        ]
      }
    }
    "#;

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

    #[test]
    fn catalogue_documents_cannot_downgrade_to_http() {
        let error =
            RemoteConfig::resolve_catalogue_url("http://example.com/soft.json").unwrap_err();
        assert!(error.to_string().contains("HTTPS"));
    }

    #[test]
    fn v3_catalogue_maps_to_existing_configuration_contract() {
        let catalog: V3CatalogResponse = serde_json::from_str(V3_FIXTURE).unwrap();
        let (pe, systems, software, easy, gpu) =
            RemoteConfig::v3_catalog_to_contents(catalog).unwrap();

        let pe_content = pe.unwrap();
        let systems_content = systems.unwrap();
        let pe = ConfigManager::parse_pe_list(&pe_content);
        assert_eq!(pe.len(), 1);
        assert_eq!(
            pe[0].sha256.as_deref(),
            Some("BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD")
        );

        let systems = ConfigManager::parse_system_list(&systems_content);
        assert_eq!(systems.len(), 1);
        assert_eq!(systems[0].display_name, "Windows 11");

        let manager = ConfigManager::load_from_content_full_with_gpu(
            Some(&systems_content),
            Some(&pe_content),
            software.as_deref(),
            easy.as_deref(),
            gpu.as_deref(),
        );
        assert_eq!(manager.software_list.len(), 1);
        assert_eq!(manager.gpu_driver_list.len(), 1);
        assert_eq!(
            manager
                .easy_mode_config
                .as_ref()
                .unwrap()
                .get_systems()
                .len(),
            1
        );
    }

    #[test]
    fn v3_catalogue_rejects_unknown_schema_version() {
        let mut value: serde_json::Value = serde_json::from_str(V3_FIXTURE).unwrap();
        value["schema_version"] = serde_json::json!(4);
        let catalog: V3CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v3_catalog_to_contents(catalog).is_err());
    }
}
