//! 应用配置模块
//! 管理 config.json 配置文件，用于存储用户偏好设置

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::utils::path::get_exe_dir;

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// 小白模式是否启用
    #[serde(default)]
    pub easy_mode_enabled: bool,
    
    /// 是否已关闭小白模式提示（在非小白模式下显示的提示）
    #[serde(default)]
    pub easy_mode_tip_dismissed: bool,
    
    /// 是否已关闭小白模式下的设置提示
    #[serde(default)]
    pub easy_mode_settings_tip_dismissed: bool,
}

impl AppConfig {
    /// 获取配置文件路径
    fn get_config_path() -> PathBuf {
        get_exe_dir().join("config.json")
    }
    
    /// 从文件加载配置
    /// 如果文件不存在或解析失败，返回默认配置
    pub fn load() -> Self {
        let config_path = Self::get_config_path();
        
        if !config_path.exists() {
            log::info!("配置文件不存在，使用默认配置");
            return Self::default();
        }
        
        match std::fs::read_to_string(&config_path) {
            Ok(content) => {
                match serde_json::from_str::<AppConfig>(&content) {
                    Ok(config) => {
                        log::info!("加载配置文件成功");
                        config
                    }
                    Err(e) => {
                        log::warn!("解析配置文件失败: {}，使用默认配置", e);
                        Self::default()
                    }
                }
            }
            Err(e) => {
                log::warn!("读取配置文件失败: {}，使用默认配置", e);
                Self::default()
            }
        }
    }
    
    /// 保存配置到文件
    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = Self::get_config_path();
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&config_path, content)?;
        log::info!("配置文件已保存");
        Ok(())
    }
    
    /// 设置小白模式状态并保存
    pub fn set_easy_mode(&mut self, enabled: bool) {
        self.easy_mode_enabled = enabled;
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }
    
    /// 关闭小白模式提示
    pub fn dismiss_easy_mode_tip(&mut self) {
        self.easy_mode_tip_dismissed = true;
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }
    
    /// 关闭小白模式下的设置提示
    pub fn dismiss_easy_mode_settings_tip(&mut self) {
        self.easy_mode_settings_tip_dismissed = true;
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }
}

/// 获取当前Windows用户名
#[cfg(windows)]
pub fn get_current_username() -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    
    // 尝试从环境变量获取
    if let Ok(username) = std::env::var("USERNAME") {
        if !username.is_empty() && username.to_lowercase() != "system" {
            return Some(username);
        }
    }
    
    // 使用Windows API获取
    unsafe {
        #[link(name = "advapi32")]
        extern "system" {
            fn GetUserNameW(lpBuffer: *mut u16, pcbBuffer: *mut u32) -> i32;
        }
        
        let mut buffer = [0u16; 256];
        let mut size = buffer.len() as u32;
        
        if GetUserNameW(buffer.as_mut_ptr(), &mut size) != 0 {
            let username = OsString::from_wide(&buffer[..size as usize - 1]);
            if let Some(name) = username.to_str() {
                if !name.is_empty() && name.to_lowercase() != "system" {
                    return Some(name.to_string());
                }
            }
        }
    }
    
    None
}

#[cfg(not(windows))]
pub fn get_current_username() -> Option<String> {
    std::env::var("USER").ok()
}
