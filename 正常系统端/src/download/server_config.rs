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
use serde::Deserialize;

/// v3 单文件资源目录。正常情况下只需要一次 HTTP 请求。
pub const SERVER_V3_URL: &str = "https://letrecovery.cloud-pe.cn/v3/index.json";

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
    /// 只读取固定的 v3 单文件目录。请求或解析失败时直接返回错误，
    /// 不再静默回退到旧版 v2 多文件目录。
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

    /// 获取 v3 单文件目录。v3 是唯一受支持的远程目录协议。
    fn fetch_config() -> Result<RemoteConfigContents> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context(tr!("创建 HTTP 客户端失败"))?;

        let contents = Self::fetch_v3_config(&client).context(tr!("v3 远程资源目录不可用"))?;
        log::info!("远程资源目录已通过 v3 单请求加载");
        Ok(contents)
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
