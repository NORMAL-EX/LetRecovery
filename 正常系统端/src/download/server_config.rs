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
    SystemImageMode,
);

/// 服务端控制系统镜像目录来源的模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SystemImageMode {
    /// 每次启动从微软 MCT 产品目录获取当前正式版长期 ESD。
    Microsoft = 1,
    /// 只使用 v3 API 的 `data.system_images`。
    #[default]
    Api = 2,
    /// 微软官方镜像在前，随后合并 v3 API 镜像。
    MicrosoftAndApi = 3,
}

impl TryFrom<u8> for SystemImageMode {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Microsoft),
            2 => Ok(Self::Api),
            3 => Ok(Self::MicrosoftAndApi),
            _ => anyhow::bail!("unsupported system image mode: {value}"),
        }
    }
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct V3CatalogResponse {
    schema_version: u32,
    /// 兼容服务端把模式放在根对象的早期实现；正式位置是 `data` 内。
    #[serde(
        default,
        alias = "mode",
        deserialize_with = "deserialize_optional_mode"
    )]
    system_image_mode: Option<u8>,
    data: V3CatalogData,
}

#[derive(Debug, Deserialize)]
struct V3CatalogData {
    pe: Vec<V3PeEntry>,
    #[serde(default)]
    system_images: Vec<V3SystemEntry>,
    easy_mode: EasyModeConfig,
    software: Vec<V3SoftwareEntry>,
    gpu_drivers: Vec<V3GpuDriverEntry>,
    /// 1=微软官方，2=API，3=微软官方+API；缺失时必须保持模式1。
    #[serde(
        default,
        alias = "mode",
        deserialize_with = "deserialize_optional_mode"
    )]
    system_image_mode: Option<u8>,
}

fn deserialize_optional_mode<'de, D>(deserializer: D) -> std::result::Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .and_then(|number| u8::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("system image mode must fit in u8")),
        Some(serde_json::Value::String(text)) => text
            .trim()
            .parse::<u8>()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("system image mode string must be numeric")),
        Some(_) => Err(serde::de::Error::custom(
            "system image mode must be a number or numeric string",
        )),
    }
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
    /// 本次目录实际采用的系统镜像来源模式。
    pub system_image_mode: SystemImageMode,
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
            Ok((
                pe_content,
                dl_content,
                soft_content,
                easy_content,
                gpu_content,
                system_image_mode,
            )) => {
                config.pe_content = pe_content;
                config.dl_content = dl_content;
                config.soft_content = soft_content;
                config.easy_content = easy_content;
                config.gpu_content = gpu_content;
                config.system_image_mode = system_image_mode;
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

        let mut contents = Self::fetch_v3_config(&client).context(tr!("v3 远程资源目录不可用"))?;
        let api_systems = contents
            .1
            .as_deref()
            .map(crate::download::config::ConfigManager::parse_system_list)
            .unwrap_or_default();
        let mode = contents.5;
        let resolved_systems = match mode {
            SystemImageMode::Microsoft => {
                crate::download::microsoft_catalog::fetch_current_systems()
                    .context(tr!("无法获取微软官方系统镜像目录"))?
            }
            SystemImageMode::Api => api_systems,
            SystemImageMode::MicrosoftAndApi => {
                match crate::download::microsoft_catalog::fetch_current_systems() {
                    Ok(official) => merge_system_images(official, api_systems),
                    Err(error) if !api_systems.is_empty() => {
                        log::warn!(
                            "微软官方系统镜像目录暂时不可用，模式3继续使用 API 目录: {error:#}"
                        );
                        api_systems
                    }
                    Err(error) => {
                        return Err(error).context(tr!("微软官方和 API 系统镜像目录均不可用"))
                    }
                }
            }
        };
        if resolved_systems.is_empty() {
            anyhow::bail!("{}", tr!("系统镜像目录为空"));
        }
        contents.1 = Some(
            serde_json::to_string(&resolved_systems)
                .context("serialize resolved system image catalogue")?,
        );
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

        let mode = SystemImageMode::try_from(
            catalog
                .data
                .system_image_mode
                .or(catalog.system_image_mode)
                .unwrap_or(SystemImageMode::Api as u8),
        )?;
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
        if pe_list.is_empty() {
            anyhow::bail!("v3 catalogue must contain an enabled PE entry");
        }
        if matches!(mode, SystemImageMode::Api) && system_list.is_empty() {
            anyhow::bail!("system image mode 2 requires an enabled API system image entry");
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
            mode,
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

fn merge_system_images(official: Vec<OnlineSystem>, api: Vec<OnlineSystem>) -> Vec<OnlineSystem> {
    let mut merged = Vec::with_capacity(official.len() + api.len());
    for system in official.into_iter().chain(api) {
        let url_identity = system
            .download_url
            .split(['?', '#'])
            .next()
            .unwrap_or(&system.download_url)
            .to_ascii_lowercase();
        let display_identity = system.display_name.trim().to_ascii_lowercase();
        let duplicate = merged.iter().any(|existing: &OnlineSystem| {
            existing
                .display_name
                .trim()
                .eq_ignore_ascii_case(&display_identity)
                && existing
                    .download_url
                    .split(['?', '#'])
                    .next()
                    .unwrap_or(&existing.download_url)
                    .eq_ignore_ascii_case(&url_identity)
        });
        if !duplicate {
            merged.push(system);
        }
    }
    merged
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
        let (pe, systems, software, easy, gpu, mode) =
            RemoteConfig::v3_catalog_to_contents(catalog).unwrap();
        assert_eq!(mode, SystemImageMode::Api);

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

    #[test]
    fn system_image_mode_defaults_to_api_when_absent() {
        let catalog: V3CatalogResponse = serde_json::from_str(V3_FIXTURE).unwrap();
        assert_eq!(
            RemoteConfig::v3_catalog_to_contents(catalog).unwrap().5,
            SystemImageMode::Api
        );
    }

    #[test]
    fn default_api_mode_requires_an_api_system_image() {
        let mut value: serde_json::Value = serde_json::from_str(V3_FIXTURE).unwrap();
        value["data"]
            .as_object_mut()
            .unwrap()
            .remove("system_images");
        let catalog: V3CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v3_catalog_to_contents(catalog).is_err());
    }

    #[test]
    fn data_system_image_mode_takes_precedence_over_root_compatibility_field() {
        let mut value: serde_json::Value = serde_json::from_str(V3_FIXTURE).unwrap();
        value["system_image_mode"] = serde_json::json!(2);
        value["data"]["system_image_mode"] = serde_json::json!(3);
        let catalog: V3CatalogResponse = serde_json::from_value(value).unwrap();
        assert_eq!(
            RemoteConfig::v3_catalog_to_contents(catalog).unwrap().5,
            SystemImageMode::MicrosoftAndApi
        );
    }

    #[test]
    fn invalid_system_image_mode_is_rejected() {
        let mut value: serde_json::Value = serde_json::from_str(V3_FIXTURE).unwrap();
        value["data"]["system_image_mode"] = serde_json::json!(9);
        let catalog: V3CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v3_catalog_to_contents(catalog).is_err());
    }

    #[test]
    fn mode_two_requires_an_api_system_image() {
        let mut value: serde_json::Value = serde_json::from_str(V3_FIXTURE).unwrap();
        value["data"]["system_image_mode"] = serde_json::json!(2);
        value["data"]["system_images"] = serde_json::json!([]);
        let catalog: V3CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v3_catalog_to_contents(catalog).is_err());
    }

    #[test]
    fn mode_three_merges_official_and_api_without_exact_duplicates() {
        let official = OnlineSystem {
            download_url: "http://dl.delivery.mp.microsoft.com/files/windows.esd".into(),
            display_name: "Windows 11 25H2 官方原版".into(),
            is_win11: true,
            filename: Some("windows.esd".into()),
            md5: None,
            sha256: None,
        };
        let mut duplicate = official.clone();
        duplicate.download_url =
            "http://dl.delivery.mp.microsoft.com/files/windows.esd?ignored=one".into();
        let api_only = OnlineSystem {
            download_url: "https://example.com/windows.esd".into(),
            display_name: "Windows 11 专业版".into(),
            is_win11: true,
            filename: Some("windows.esd".into()),
            md5: None,
            sha256: None,
        };
        let merged = merge_system_images(vec![official], vec![duplicate, api_only]);
        assert_eq!(merged.len(), 2);
        assert!(merged[0].display_name.contains("25H2"));
        assert_eq!(merged[1].display_name, "Windows 11 专业版");
    }

    #[test]
    #[ignore = "requires the live LetRecovery v3 catalogue service"]
    fn live_missing_mode_defaults_to_the_api_catalogue() {
        let config = RemoteConfig::load_from_server();
        assert!(config.loaded, "{:?}", config.error);
        assert_eq!(config.system_image_mode, SystemImageMode::Api);
        let systems = ConfigManager::parse_system_list(config.dl_content.as_deref().unwrap());
        assert!(systems.len() >= 20);
        assert!(systems.iter().any(|system| system.is_win11));
        assert!(systems.iter().any(|system| !system.is_win11));
    }
}
