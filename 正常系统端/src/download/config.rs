use anyhow::Result;
use serde::{Deserialize, Serialize};

/// 在线系统镜像信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSystem {
    pub download_url: String,
    pub display_name: String,
    pub is_win11: bool,
}

/// 在线 PE 信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlinePE {
    pub download_url: String,
    pub display_name: String,
    pub filename: String,
    /// MD5校验值（可选）
    #[serde(default)]
    pub md5: Option<String>,
}

/// 本地缓存的PE配置（不含下载链接）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPE {
    pub display_name: String,
    pub filename: String,
    /// MD5校验值（可选）
    #[serde(default)]
    pub md5: Option<String>,
}

impl From<&OnlinePE> for CachedPE {
    fn from(pe: &OnlinePE) -> Self {
        Self {
            display_name: pe.display_name.clone(),
            filename: pe.filename.clone(),
            md5: pe.md5.clone(),
        }
    }
}

impl CachedPE {
    /// 转换为OnlinePE（下载链接设为空）
    pub fn to_online_pe(&self) -> OnlinePE {
        OnlinePE {
            download_url: String::new(),
            display_name: self.display_name.clone(),
            filename: self.filename.clone(),
            md5: self.md5.clone(),
        }
    }
}

/// PE配置缓存文件结构
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeCache {
    pub pe_list: Vec<CachedPE>,
    pub version: u32,
}

impl PeCache {
    const CACHE_VERSION: u32 = 1;
    
    /// 获取缓存文件路径
    fn get_cache_path() -> std::path::PathBuf {
        crate::utils::path::get_exe_dir().join("pe_cache.json")
    }
    
    /// 保存PE配置到本地缓存（不包含下载链接）
    pub fn save(pe_list: &[OnlinePE]) -> Result<()> {
        let cache = PeCache {
            pe_list: pe_list.iter().map(CachedPE::from).collect(),
            version: Self::CACHE_VERSION,
        };
        
        let cache_path = Self::get_cache_path();
        let json_content = serde_json::to_string_pretty(&cache)?;
        std::fs::write(&cache_path, json_content)?;
        
        log::info!("PE配置已缓存到: {:?}, 共 {} 项", cache_path, pe_list.len());
        Ok(())
    }
    
    /// 从本地缓存加载PE配置
    pub fn load() -> Option<Vec<OnlinePE>> {
        let cache_path = Self::get_cache_path();
        
        if !cache_path.exists() {
            log::info!("PE缓存文件不存在: {:?}", cache_path);
            return None;
        }
        
        match std::fs::read_to_string(&cache_path) {
            Ok(content) => {
                match serde_json::from_str::<PeCache>(&content) {
                    Ok(cache) => {
                        if cache.version != Self::CACHE_VERSION {
                            log::warn!("PE缓存版本不匹配，忽略缓存");
                            return None;
                        }
                        
                        let pe_list: Vec<OnlinePE> = cache.pe_list
                            .iter()
                            .map(|c| c.to_online_pe())
                            .collect();
                        
                        log::info!("从缓存加载PE配置，共 {} 项", pe_list.len());
                        Some(pe_list)
                    }
                    Err(e) => {
                        log::warn!("解析PE缓存失败: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("读取PE缓存文件失败: {}", e);
                None
            }
        }
    }
    
    /// 检查是否有本地PE可用（已下载过）
    pub fn has_downloaded_pe(filename: &str) -> bool {
        let (exists, _) = crate::core::pe::PeManager::check_pe_exists(filename);
        exists
    }
}

/// 在线软件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSoftware {
    /// 软件名称
    pub name: String,
    /// 软件描述
    pub description: String,
    /// 更新日期
    pub update_date: String,
    /// 文件大小
    pub file_size: String,
    /// 图标URL（可选）
    #[serde(default)]
    pub icon_url: Option<String>,
    /// 下载URL（64位）
    pub download_url: String,
    /// 下载URL（32位，可选）
    #[serde(default)]
    pub download_url_x86: Option<String>,
    /// XP系统下载URL（可选）
    #[serde(default)]
    pub download_url_nt5: Option<String>,
    /// 文件名
    pub filename: String,
}

/// 软件列表JSON格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftwareList {
    pub software: Vec<OnlineSoftware>,
}

/// 配置管理器
#[derive(Debug, Clone, Default)]
pub struct ConfigManager {
    pub systems: Vec<OnlineSystem>,
    pub pe_list: Vec<OnlinePE>,
    pub software_list: Vec<OnlineSoftware>,
    /// 小白模式配置
    pub easy_mode_config: Option<EasyModeConfig>,
}

impl ConfigManager {
    /// 从远程服务器加载配置
    pub async fn load_from_remote(system_url: &str, pe_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;

        // 下载系统列表
        let systems = if let Ok(resp) = client.get(system_url).send().await {
            if let Ok(text) = resp.text().await {
                Self::parse_system_list(&text)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // 下载 PE 列表
        let pe_list = if let Ok(resp) = client.get(pe_url).send().await {
            if let Ok(text) = resp.text().await {
                Self::parse_pe_list(&text)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        Ok(Self { systems, pe_list, software_list: Vec::new(), easy_mode_config: None })
    }
    
    /// 从远程配置内容加载
    /// 
    /// # Arguments
    /// * `dl_content` - 系统镜像列表内容
    /// * `pe_content` - PE 列表内容
    pub fn load_from_content(dl_content: Option<&str>, pe_content: Option<&str>) -> Self {
        let systems = dl_content
            .map(|c| Self::parse_system_list(c))
            .unwrap_or_default();
        
        let pe_list = pe_content
            .map(|c| Self::parse_pe_list(c))
            .unwrap_or_default();
        
        Self { systems, pe_list, software_list: Vec::new(), easy_mode_config: None }
    }
    
    /// 从远程配置内容加载（包含软件列表）
    /// 
    /// # Arguments
    /// * `dl_content` - 系统镜像列表内容
    /// * `pe_content` - PE 列表内容
    /// * `soft_content` - 软件列表内容（JSON格式）
    pub fn load_from_content_with_soft(
        dl_content: Option<&str>, 
        pe_content: Option<&str>,
        soft_content: Option<&str>,
    ) -> Self {
        let systems = dl_content
            .map(|c| Self::parse_system_list(c))
            .unwrap_or_default();
        
        let pe_list = pe_content
            .map(|c| Self::parse_pe_list(c))
            .unwrap_or_default();
        
        let software_list = soft_content
            .map(|c| Self::parse_software_list(c))
            .unwrap_or_default();
        
        Self { systems, pe_list, software_list, easy_mode_config: None }
    }
    
    /// 从远程配置内容加载（完整版，包含所有配置）
    /// 
    /// # Arguments
    /// * `dl_content` - 系统镜像列表内容
    /// * `pe_content` - PE 列表内容
    /// * `soft_content` - 软件列表内容（JSON格式）
    /// * `easy_content` - 小白模式配置内容（JSON格式）
    pub fn load_from_content_full(
        dl_content: Option<&str>, 
        pe_content: Option<&str>,
        soft_content: Option<&str>,
        easy_content: Option<&str>,
    ) -> Self {
        let systems = dl_content
            .map(|c| Self::parse_system_list(c))
            .unwrap_or_default();
        
        let pe_list = pe_content
            .map(|c| Self::parse_pe_list(c))
            .unwrap_or_default();
        
        let software_list = soft_content
            .map(|c| Self::parse_software_list(c))
            .unwrap_or_default();
        
        let easy_mode_config = easy_content
            .and_then(|c| EasyModeConfig::parse(c));
        
        Self { systems, pe_list, software_list, easy_mode_config }
    }

    /// 解析系统列表
    /// 格式: URL,显示名称,Win11/Win10
    pub fn parse_system_list(content: &str) -> Vec<OnlineSystem> {
        content
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 3 {
                    Some(OnlineSystem {
                        download_url: parts[0].trim().to_string(),
                        display_name: parts[1].trim().to_string(),
                        is_win11: parts[2].trim().eq_ignore_ascii_case("Win11"),
                    })
                } else if parts.len() >= 2 {
                    Some(OnlineSystem {
                        download_url: parts[0].trim().to_string(),
                        display_name: parts[1].trim().to_string(),
                        is_win11: parts[1].to_lowercase().contains("11"),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// 解析 PE 列表
    /// 格式: URL,显示名称,文件名[,MD5]
    pub fn parse_pe_list(content: &str) -> Vec<OnlinePE> {
        content
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 4 {
                    // 4字段格式: URL,显示名称,文件名,MD5
                    let md5_str = parts[3].trim();
                    let md5 = if md5_str.is_empty() {
                        None
                    } else {
                        Some(md5_str.to_uppercase())
                    };
                    Some(OnlinePE {
                        download_url: parts[0].trim().to_string(),
                        display_name: parts[1].trim().to_string(),
                        filename: parts[2].trim().to_string(),
                        md5,
                    })
                } else if parts.len() >= 3 {
                    Some(OnlinePE {
                        download_url: parts[0].trim().to_string(),
                        display_name: parts[1].trim().to_string(),
                        filename: parts[2].trim().to_string(),
                        md5: None,
                    })
                } else if parts.len() >= 2 {
                    let url = parts[0].trim();
                    let filename = url.split('/').last().unwrap_or("pe.wim").to_string();
                    Some(OnlinePE {
                        download_url: url.to_string(),
                        display_name: parts[1].trim().to_string(),
                        filename,
                        md5: None,
                    })
                } else {
                    None
                }
            })
            .collect()
    }
    
    /// 解析软件列表（JSON格式）
    pub fn parse_software_list(content: &str) -> Vec<OnlineSoftware> {
        match serde_json::from_str::<SoftwareList>(content) {
            Ok(list) => list.software,
            Err(e) => {
                log::warn!("解析软件列表失败: {}", e);
                Vec::new()
            }
        }
    }

    /// 检查配置是否为空
    pub fn is_empty(&self) -> bool {
        self.systems.is_empty() && self.pe_list.is_empty()
    }
    
    /// 检查软件列表是否为空
    pub fn has_software(&self) -> bool {
        !self.software_list.is_empty()
    }
}

/// 小白模式分卷信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EasyModeVolume {
    /// 实际分卷号（WIM索引）
    pub number: u32,
    /// 分卷显示名称
    pub name: String,
}

/// 小白模式系统信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EasyModeSystem {
    /// 系统Logo URL
    pub os_logo: String,
    /// 系统下载链接
    pub os_download: String,
    /// 分卷列表
    pub volume: Vec<EasyModeVolume>,
}

/// 小白模式配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EasyModeConfig {
    /// 系统列表，每个元素是一个HashMap，键为系统名称
    pub system: Vec<std::collections::HashMap<String, EasyModeSystem>>,
}

impl EasyModeConfig {
    /// 从JSON字符串解析
    pub fn parse(content: &str) -> Option<Self> {
        match serde_json::from_str::<EasyModeConfig>(content) {
            Ok(config) => {
                log::info!("小白模式配置加载成功，共 {} 个系统", config.system.len());
                Some(config)
            }
            Err(e) => {
                log::warn!("解析小白模式配置失败: {}", e);
                None
            }
        }
    }
    
    /// 获取所有系统（展平为Vec）
    pub fn get_systems(&self) -> Vec<(String, EasyModeSystem)> {
        self.system
            .iter()
            .flat_map(|map| {
                map.iter().map(|(name, sys)| (name.clone(), sys.clone()))
            })
            .collect()
    }
}
