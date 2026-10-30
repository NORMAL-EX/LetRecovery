//! 服务器配置模块
//! 从远程服务器获取 PE 和系统镜像配置

use crate::{
    download::config::{
        EasyModeConfig, OnlinePE, OnlineSoftware, OnlineSystem, SoftwareCategory,
        SoftwareCategoryList,
    },
    tr,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// v4 单文件资源目录。正常情况下只需要一次 HTTP 请求。
pub const SERVER_V4_URL: &str = lr_core::branding::CATALOG_URL;

type RemoteConfigContents = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    SystemImageMode,
    HashMap<String, HashMap<String, String>>,
);

/// 服务端控制系统镜像目录来源的模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SystemImageMode {
    /// 每次启动从微软 MCT 产品目录获取当前正式版长期 ESD。
    Microsoft = 1,
    /// 只使用 v4 API 的 `data.system_images`。
    #[default]
    Api = 2,
    /// 微软官方镜像在前，随后合并 v4 API 镜像。
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
struct V4CatalogResponse {
    data: V4CatalogData,
    /// Also accept a sibling field for API deployments that merge the map at response level.
    #[serde(default)]
    i18n: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct V4CatalogData {
    pe: Vec<V4PeEntry>,
    #[serde(default)]
    system_images: Vec<V4SystemEntry>,
    easy_mode: EasyModeConfig,
    software: Vec<V4SoftwareCategory>,
    /// Optional source-text -> locale -> translation table returned alongside the v4 catalogue.
    #[serde(default)]
    i18n: Option<Value>,
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
struct V4PeEntry {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(flatten)]
    value: OnlinePE,
}

#[derive(Debug, Deserialize)]
struct V4SystemEntry {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    download_url: String,
    display_name: String,
    filename: Option<String>,
    #[serde(default)]
    md5: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    os: Option<u8>,
    #[serde(default)]
    legacy_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct V4SoftwareCategory {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    items: Vec<OnlineSoftware>,
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
    /// 旧调用链兼容字段；v4 不再提供独立显卡驱动目录。
    pub gpu_content: Option<String>,
    /// 是否加载成功
    pub loaded: bool,
    /// 错误信息
    pub error: Option<String>,
    /// 本次目录实际采用的系统镜像来源模式。
    pub system_image_mode: SystemImageMode,
    /// Server-maintained translations for display strings supplied by the v4 catalogue.
    pub catalogue_translations: HashMap<String, HashMap<String, String>>,
}

/// Whether a TLS failure was caused by the certificate validity window, i.e. by a wrong local
/// clock: Schannel `CERT_E_EXPIRED` (0x800B0101, reported as os error -2146762495) or the rustls
/// "not valid yet" / "expired" certificate errors.
fn is_clock_related_tls_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "-2146762495",
        "0x800b0101",
        "certificate not valid yet",
        "certificate expired",
        "not valid before",
        "not valid after",
        "证书不在有效期内",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

impl RemoteConfig {
    /// 从服务器加载配置
    ///
    /// 只读取固定的 v4 单文件目录。请求或解析失败时直接返回错误，
    /// 不再静默回退到旧版 v2 多文件目录。
    pub fn load_from_server() -> Self {
        let mut config = RemoteConfig::default();

        // 尝试加载配置
        let fetched = match Self::fetch_config() {
            Err(error) if is_clock_related_tls_error(&format!("{error:#}")) => {
                Self::fetch_after_time_sync(error)
            }
            result => result,
        };
        match fetched {
            Ok((
                pe_content,
                dl_content,
                soft_content,
                easy_content,
                gpu_content,
                system_image_mode,
                catalogue_translations,
            )) => {
                config.pe_content = pe_content;
                config.dl_content = dl_content;
                config.soft_content = soft_content;
                config.easy_content = easy_content;
                config.gpu_content = gpu_content;
                config.system_image_mode = system_image_mode;
                config.catalogue_translations = catalogue_translations;
                config.loaded = true;
                log::info!("远程配置加载成功");
            }
            Err(e) => {
                config.error = Some(format!("{e:#}"));
                config.loaded = false;
                log::warn!("远程配置加载失败: {e:#}");
            }
        }

        config
    }

    /// 获取 v4 单文件目录。v4 是唯一受支持的远程目录协议。
    fn fetch_config() -> Result<RemoteConfigContents> {
        let native_client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context(tr!("创建 HTTP 客户端失败"))?;

        let mut contents = match Self::fetch_v4_config(&native_client) {
            Ok(contents) => contents,
            Err(native_error) => {
                // Windows 7 frequently has a stale WinHTTP/IE proxy or a Schannel installation
                // without a currently usable TLS credential chain. This retry is deliberately
                // limited to LetRecovery's fixed HTTPS catalogue: it bypasses the machine proxy
                // and uses the bundled WebPKI root set, without weakening certificate checks or
                // changing the transport policy for user-provided URLs.
                log::warn!(
                    "系统 TLS/代理路径无法读取固定 v4 目录，改用直连 Rustls 重试: {native_error:#}"
                );
                let direct_client = reqwest::blocking::Client::builder()
                    .use_rustls_tls()
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .context(tr!("创建兼容 HTTP 客户端失败"))?;
                Self::fetch_v4_config(&direct_client).map_err(|direct_error| {
                    anyhow::anyhow!(
                        "{}; system TLS/proxy error: {native_error:#}; direct rustls error: {direct_error:#}",
                        tr!("v4 远程资源目录不可用")
                    )
                })?
            }
        };
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
        log::info!("远程资源目录已通过 v4 单请求加载");
        Ok(contents)
    }

    /// A certificate that is "not yet valid" or "expired" almost always means the local clock is
    /// wrong (a VM resumed from an old snapshot, an empty CMOS battery). Synchronise the clock once
    /// and fetch once more; when that is impossible, report exactly why instead of a bare TLS error.
    fn fetch_after_time_sync(first: anyhow::Error) -> Result<RemoteConfigContents> {
        log::warn!(
            "远程配置的 HTTPS 证书有效期校验失败，系统时间可能不正确，正在同步时间后重试一次: {first:#}"
        );
        let sync = crate::core::tool_time_sync::sync_time_to_beijing();
        if !sync.success {
            anyhow::bail!(
                "系统时间可能不正确（HTTPS 证书有效期校验失败），自动同步时间也失败：{}。请手动校准系统时间后重试。原始错误：{first:#}",
                sync.message
            );
        }
        log::info!(
            "系统时间已同步（{} -> {}），重新获取远程配置",
            sync.old_time.as_deref().unwrap_or("?"),
            sync.new_time.as_deref().unwrap_or("?")
        );
        Self::fetch_config().map_err(|retry| {
            anyhow::anyhow!(
                "系统时间已同步，但重新获取远程配置仍失败：{retry:#}；同步前的错误：{first:#}"
            )
        })
    }

    fn fetch_v4_config(client: &reqwest::blocking::Client) -> Result<RemoteConfigContents> {
        log::info!("请求 v4 服务器配置: {}", SERVER_V4_URL);
        let response = client
            .get(SERVER_V4_URL)
            .send()
            .context(tr!("请求服务器配置失败"))?;

        if !response.status().is_success() {
            anyhow::bail!("{}", tr!("服务器返回错误状态码: {}", response.status()));
        }

        let catalog: V4CatalogResponse = response.json().context(tr!("解析服务器响应失败"))?;
        Self::v4_catalog_to_contents(catalog)
    }

    fn v4_catalog_to_contents(catalog: V4CatalogResponse) -> Result<RemoteConfigContents> {
        let catalogue_translations =
            parse_catalogue_translations(catalog.data.i18n.as_ref().or(catalog.i18n.as_ref()));
        let mode = SystemImageMode::try_from(
            catalog
                .data
                .system_image_mode
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
            .map(|entry| OnlineSystem {
                download_url: entry.download_url,
                display_name: entry.display_name,
                is_win11: entry.os == Some(11)
                    || entry
                        .legacy_type
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case("win11")),
                filename: entry.filename,
                md5: entry.md5,
                sha256: entry.sha256,
            })
            .collect();
        if pe_list.is_empty() {
            anyhow::bail!("v4 catalogue must contain an enabled PE entry");
        }
        if matches!(mode, SystemImageMode::Api) && system_list.is_empty() {
            anyhow::bail!("system image mode 2 requires an enabled API system image entry");
        }

        let categories: Vec<SoftwareCategory> = catalog
            .data
            .software
            .into_iter()
            .map(|entry| SoftwareCategory {
                id: entry.id,
                name: entry.name,
                description: entry.description,
                items: entry.items,
            })
            .filter(|entry| !entry.items.is_empty())
            .collect();

        let pe_content = serde_json::to_string(&pe_list).context("serialize v4 PE catalogue")?;
        let dl_content =
            serde_json::to_string(&system_list).context("serialize v4 system catalogue")?;
        let soft_content = serde_json::to_string(&SoftwareCategoryList { categories })
            .context("serialize v4 software catalogue")?;
        let easy_content = serde_json::to_string(&catalog.data.easy_mode)
            .context("serialize v4 easy-mode catalogue")?;

        Ok((
            Some(pe_content),
            Some(dl_content),
            Some(soft_content),
            Some(easy_content),
            None,
            mode,
            catalogue_translations,
        ))
    }
}

const CATALOGUE_I18N_MAX_ENTRIES: usize = 4096;
const CATALOGUE_I18N_MAX_TEXT_BYTES: usize = 4096;
const CATALOGUE_I18N_LOCALES: [&str; 5] = ["en-US", "de-DE", "fr-FR", "ja-JP", "ko-KR"];

/// Invalid optional translation rows are ignored so a bad i18n field cannot make the core v4
/// resource catalogue unavailable.
fn parse_catalogue_translations(value: Option<&Value>) -> HashMap<String, HashMap<String, String>> {
    let Some(entries) = value.and_then(Value::as_object) else {
        if value.is_some() {
            log::warn!("v4 catalogue i18n field is not an object; translations ignored");
        }
        return HashMap::new();
    };
    let mut translations = HashMap::new();
    for (source, locale_values) in entries.iter().take(CATALOGUE_I18N_MAX_ENTRIES) {
        if source.trim().is_empty() || source.len() > CATALOGUE_I18N_MAX_TEXT_BYTES {
            continue;
        }
        let Some(locale_values) = locale_values.as_object() else {
            continue;
        };
        let mut locales = HashMap::new();
        for (locale, translated) in locale_values {
            if !CATALOGUE_I18N_LOCALES
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(locale))
            {
                continue;
            }
            let Some(translated) = translated.as_str() else {
                continue;
            };
            if translated.trim().is_empty() || translated.len() > CATALOGUE_I18N_MAX_TEXT_BYTES {
                continue;
            }
            locales.insert(locale.clone(), translated.to_owned());
        }
        if !locales.is_empty() {
            translations.insert(source.clone(), locales);
        }
    }
    if entries.len() > CATALOGUE_I18N_MAX_ENTRIES {
        log::warn!("v4 catalogue i18n has too many entries; excess rows ignored");
    }
    translations
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

    const V4_FIXTURE: &str = r#"
    {
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
            "legacy_type": "Win11",
            "filename": "windows-11.esd",
            "os": 11,
            "enabled": true
          },
          {
            "download_url": "https://example.com/disabled.esd",
            "display_name": "Disabled",
            "legacy_type": "Win10",
            "filename": "disabled.esd",
            "os": 10,
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
            "id": "utility",
            "name": "常用软件",
            "description": "常用工具集合",
            "count": 1,
            "items": [{
              "id": "tool",
              "name": "工具",
              "description": "工具描述",
              "download_url": "https://example.com/tool.exe",
              "filename": "tool.exe",
              "version": "1.0",
              "silent_command": "\"{installer}\" /S",
            "requires_admin": true
            ,"vm_tools": false
            }]
          }
        ],
        "system_image_mode": 2,
        "i18n": {
          "常用软件": {"en-US": "Common software"},
          "工具": {"en-US": "Utility"},
          "工具描述": {"en-US": "A utility application"}
        }
      }
    }
    "#;

    #[test]
    fn certificate_validity_failures_are_recognised_as_clock_problems() {
        let schannel = "client error (Connect): (os error -2146762495)";
        let schannel_zh = "要求的证书不在有效期内。";
        let rustls_not_yet = "invalid peer certificate: certificate not valid yet";
        let rustls_expired = "invalid peer certificate: certificate expired";
        let dns = "dns error: No such host is known. (os error 11001)";
        let issuer = "invalid peer certificate: UnknownIssuer";
        for clock in [schannel, schannel_zh, rustls_not_yet, rustls_expired] {
            assert!(is_clock_related_tls_error(clock), "{clock}");
        }
        for other in [dns, issuer] {
            assert!(!is_clock_related_tls_error(other), "{other}");
        }
    }

    #[test]
    fn v4_catalogue_maps_categories_and_silent_install_metadata() {
        let catalog: V4CatalogResponse = serde_json::from_str(V4_FIXTURE).unwrap();
        let (pe, systems, software, easy, gpu, mode, i18n) =
            RemoteConfig::v4_catalog_to_contents(catalog).unwrap();
        assert_eq!(mode, SystemImageMode::Api);
        assert_eq!(
            i18n.get("常用软件")
                .and_then(|locales| locales.get("en-US")),
            Some(&"Common software".to_owned())
        );

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
        assert_eq!(manager.software_categories.len(), 1);
        assert_eq!(manager.software_categories[0].name, "常用软件");
        assert_eq!(manager.software_list[0].name, "工具");
        assert_eq!(
            manager.software_list[0].silent_command.as_deref(),
            Some("\"{installer}\" /S")
        );
        assert!(manager.software_list[0].requires_admin);
        assert!(!manager.software_list[0].vm_tools);
        assert!(manager.gpu_driver_list.is_empty());
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
    fn system_image_mode_defaults_to_api_when_absent() {
        let mut value: serde_json::Value = serde_json::from_str(V4_FIXTURE).unwrap();
        value["data"]
            .as_object_mut()
            .unwrap()
            .remove("system_image_mode");
        let catalog: V4CatalogResponse = serde_json::from_value(value).unwrap();
        assert_eq!(
            RemoteConfig::v4_catalog_to_contents(catalog).unwrap().5,
            SystemImageMode::Api
        );
    }

    #[test]
    fn default_api_mode_requires_an_api_system_image() {
        let mut value: serde_json::Value = serde_json::from_str(V4_FIXTURE).unwrap();
        value["data"]
            .as_object_mut()
            .unwrap()
            .remove("system_images");
        let catalog: V4CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v4_catalog_to_contents(catalog).is_err());
    }

    #[test]
    fn invalid_system_image_mode_is_rejected() {
        let mut value: serde_json::Value = serde_json::from_str(V4_FIXTURE).unwrap();
        value["data"]["system_image_mode"] = serde_json::json!(9);
        let catalog: V4CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v4_catalog_to_contents(catalog).is_err());
    }

    #[test]
    fn invalid_optional_i18n_rows_are_ignored_without_rejecting_the_catalogue() {
        let mut value: serde_json::Value = serde_json::from_str(V4_FIXTURE).unwrap();
        value["data"]["i18n"] = serde_json::json!({
            "valid": {"en-US": "Valid"},
            "empty": {"en-US": " "},
            "unsupported": {"zh-CN": "不应出现"},
            "wrong-type": {"en-US": 42},
            "bad-row": "not a locale map"
        });
        let catalog: V4CatalogResponse = serde_json::from_value(value).unwrap();
        let translations = RemoteConfig::v4_catalog_to_contents(catalog).unwrap().6;
        assert_eq!(translations.len(), 1);
        assert_eq!(translations["valid"]["en-US"], "Valid");
    }

    #[test]
    fn mode_two_requires_an_api_system_image() {
        let mut value: serde_json::Value = serde_json::from_str(V4_FIXTURE).unwrap();
        value["data"]["system_image_mode"] = serde_json::json!(2);
        value["data"]["system_images"] = serde_json::json!([]);
        let catalog: V4CatalogResponse = serde_json::from_value(value).unwrap();
        assert!(RemoteConfig::v4_catalog_to_contents(catalog).is_err());
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
    #[ignore = "requires the live LetRecovery v4 catalogue service"]
    fn live_missing_mode_defaults_to_the_api_catalogue() {
        let config = RemoteConfig::load_from_server();
        assert!(config.loaded, "{:?}", config.error);
        assert_eq!(config.system_image_mode, SystemImageMode::Api);
        let systems = ConfigManager::parse_system_list(config.dl_content.as_deref().unwrap());
        assert!(!systems.is_empty());
        assert!(systems.iter().any(|system| system.is_win11));
        assert!(systems.iter().any(|system| !system.is_win11));
    }
}
