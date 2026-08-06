//! 应用配置模块
//! 管理 config.json 配置文件，用于存储用户偏好设置

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::utils::path::get_exe_dir;

static CONFIG_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn config_write_lock() -> &'static Mutex<()> {
    CONFIG_WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// 是否启用日志记录（默认启用）
    #[serde(default = "default_log_enabled")]
    pub log_enabled: bool,

    /// 日志保留天数（默认7天）
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,

    /// 界面语言代码（默认 "zh-CN"）
    #[serde(default = "default_language")]
    pub language: String,

    /// PE 配置缓存（原 pe_cache.json，已并入 config.json）
    #[serde(default)]
    pub pe_cache: crate::download::config::PeCache,

    /// WIM 镜像引擎：0=libwim（默认，内置），1=wimgapi（系统原生 API）
    #[serde(default)]
    pub wim_engine: u8,

    /// 旧版高级模式开关，仅用于读取旧配置；加载后固定关闭且保存时不再写回。
    #[serde(default, skip_serializing)]
    pub enable_advanced_options: bool,

    /// Compatibility switch for trusted deployments that still publish HTTP
    /// download URLs. HTTPS remains the secure default.
    #[serde(default)]
    pub allow_insecure_http_downloads: bool,

    /// 单个下载任务使用的并行分片数，只接受 8、16、32 三档。aria2 的单服务器
    /// 连接上限仍限制为 16；旧配置没有此字段时保持迁移前的 16 连接行为。
    #[serde(default = "default_download_threads")]
    pub download_threads: u8,

    /// 「系统安装」页选项偏好（记住上次勾选状态，下次启动自动恢复）。
    #[serde(default)]
    pub install_prefs: crate::core::ui_state::InstallPrefs,
}

/// 日志默认启用
fn default_log_enabled() -> bool {
    true
}

/// 日志默认保留7天
fn default_log_retention_days() -> u32 {
    7
}

/// 默认语言为简体中文
fn default_language() -> String {
    String::from("zh-CN")
}

/// 把旧配置或损坏配置归一到 UI 暴露的 8、16、32 三档。
///
/// 12 和 24 是相邻档位的中点；中点选择更高一档，避免旧的较大配置被意外降得
/// 过低。aria2 的 `max-connection-per-server` 会在执行边界单独限制为 16。
pub const fn normalize_download_threads(threads: u8) -> u8 {
    match threads {
        0..=11 => 8,
        12..=23 => 16,
        _ => 32,
    }
}

const fn default_download_threads() -> u8 {
    16
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            easy_mode_enabled: false,
            easy_mode_tip_dismissed: false,
            easy_mode_settings_tip_dismissed: false,
            log_enabled: true,               // 日志默认启用
            log_retention_days: 7,           // 默认保留7天
            language: String::from("zh-CN"), // 默认简体中文
            pe_cache: crate::download::config::PeCache::default(),
            wim_engine: 0, // 默认 libwim
            enable_advanced_options: false,
            allow_insecure_http_downloads: false,
            download_threads: default_download_threads(),
            install_prefs: crate::core::ui_state::InstallPrefs::default(),
        }
    }
}

impl AppConfig {
    /// 获取配置文件路径
    fn get_config_path() -> PathBuf {
        get_exe_dir().join("config.json")
    }

    /// 从文件加载配置
    /// 如果文件不存在或解析失败，返回默认配置
    ///
    /// 注意：此方法可能在日志系统初始化之前被调用，
    /// 因此使用 load_silent() 进行静默加载
    pub fn load() -> Self {
        Self::load_silent()
    }

    /// 静默加载配置（不输出日志）
    /// 用于在日志系统初始化之前加载配置
    fn load_silent() -> Self {
        let config_path = Self::get_config_path();

        if !config_path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(&config_path) {
            Ok(content) => serde_json::from_str::<AppConfig>(&content)
                .map(Self::normalized)
                .unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 重新加载配置并记录日志
    /// 用于在日志系统初始化之后需要重新加载时使用
    pub fn reload_with_logging() -> Self {
        let config_path = Self::get_config_path();

        if !config_path.exists() {
            log::info!("配置文件不存在，使用默认配置");
            return Self::default();
        }

        match std::fs::read_to_string(&config_path) {
            Ok(content) => match serde_json::from_str::<AppConfig>(&content) {
                Ok(config) => {
                    log::info!("加载配置文件成功");
                    config.normalized()
                }
                Err(e) => {
                    log::warn!("解析配置文件失败: {}，使用默认配置", e);
                    Self::default()
                }
            },
            Err(e) => {
                log::warn!("读取配置文件失败: {}，使用默认配置", e);
                Self::default()
            }
        }
    }

    fn write_atomic(&self, config_path: &Path) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        let directory = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config.json path has no parent directory"))?;
        let (temporary, mut file) = lr_core::scoped_temp_file::ScopedTempFile::create_writer_in(
            directory, "config", "json",
        )?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        temporary.persist_replace(config_path)?;
        log::info!("配置文件已保存");
        Ok(())
    }

    fn merge_latest_pe_cache(&self, latest: Self) -> Self {
        let mut merged = self.clone();
        merged.pe_cache = latest.pe_cache;
        merged
    }

    /// 保存普通应用配置。
    ///
    /// PE 目录由异步在线目录刷新独立维护，因此这里必须在同一写锁内重新读取并
    /// 保留磁盘上的最新 PE 缓存，避免窗口持有的旧快照把刚写入的目录覆盖为空。
    pub fn save(&self) -> anyhow::Result<()> {
        let _guard = config_write_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("config.json write lock is poisoned"))?;
        let config_path = Self::get_config_path();
        self.merge_latest_pe_cache(Self::load_silent())
            .write_atomic(&config_path)
    }

    /// 只替换 PE 目录缓存，同时保留磁盘上最新的用户偏好。
    pub(crate) fn replace_pe_cache(
        pe_cache: crate::download::config::PeCache,
    ) -> anyhow::Result<()> {
        let _guard = config_write_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("config.json write lock is poisoned"))?;
        let config_path = Self::get_config_path();
        let mut latest = Self::load_silent();
        latest.pe_cache = pe_cache;
        latest.write_atomic(&config_path)
    }

    fn normalized(mut self) -> Self {
        self.download_threads = normalize_download_threads(self.download_threads);
        // The removed global Advanced Mode and DiskPart switches remain readable for compatibility
        // but can never be re-enabled. Supported installation advanced options keep their ordinary
        // persisted preferences; sensitive and session-only fields are already `serde(skip)`.
        self.enable_advanced_options = false;
        self.install_prefs.advanced_options.apply_runtime_defaults();
        self.install_prefs.run_diskpart_scripts = false;
        self
    }

    /// 设置小白模式状态并保存
    pub fn set_easy_mode(&mut self, enabled: bool) {
        self.easy_mode_enabled = enabled;
        self.enable_advanced_options = false;
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

    /// 设置日志记录状态并保存
    pub fn set_log_enabled(&mut self, enabled: bool) {
        self.log_enabled = enabled;
        // 更新运行时状态
        crate::utils::logger::LogManager::set_enabled(enabled);
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }

    /// 设置日志保留天数并保存
    pub fn set_log_retention_days(&mut self, days: u32) {
        self.log_retention_days = days.clamp(1, 365); // 限制范围：1-365天
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }

    /// 获取日志记录状态
    pub fn is_log_enabled(&self) -> bool {
        self.log_enabled
    }

    /// 设置 WIM 镜像引擎并保存（同时更新进程级引擎选择，立即生效）
    pub fn set_wim_engine(&mut self, engine: u8) {
        self.wim_engine = engine;
        lr_core::set_active_engine(lr_core::WimEngine::from_u8(engine));
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }

    /// 将当前配置中的引擎选择应用到进程级全局（启动时调用一次）
    pub fn apply_wim_engine(&self) {
        lr_core::set_active_engine(lr_core::WimEngine::from_u8(self.wim_engine));
    }

    /// 设置单个下载任务的并行连接数。新值从下一个下载任务开始生效。
    pub fn set_download_threads(&mut self, threads: u8) {
        self.download_threads = normalize_download_threads(threads);
        if let Err(e) = self.save() {
            log::warn!("保存配置失败: {}", e);
        }
    }

    /// 设置界面语言并保存
    ///
    /// # Arguments
    /// * `language_code` - 语言代码（如 "zh-CN", "zh-TW", "en-US"）
    pub fn set_language(&mut self, language_code: &str) {
        self.language = language_code.to_string();
        // 切换运行时语言
        crate::utils::i18n::switch_language(language_code);
        if let Err(error) = crate::utils::dprk_easter_egg::sync_for_language(language_code) {
            log::warn!("同步朝鲜文彩蛋失败: {error:#}");
        }
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
        if lr_core::unattend_account::validate_unattended_local_account_name(&username).is_ok() {
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
                if lr_core::unattend_account::validate_unattended_local_account_name(name).is_ok() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_json_without_download_threads_keeps_legacy_connection_count() {
        let config: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.download_threads, 16);
    }

    #[test]
    fn download_threads_are_normalized_to_supported_tiers() {
        assert_eq!(normalize_download_threads(0), 8);
        assert_eq!(normalize_download_threads(8), 8);
        assert_eq!(normalize_download_threads(11), 8);
        assert_eq!(normalize_download_threads(12), 16);
        assert_eq!(normalize_download_threads(16), 16);
        assert_eq!(normalize_download_threads(23), 16);
        assert_eq!(normalize_download_threads(24), 32);
        assert_eq!(normalize_download_threads(32), 32);
        assert_eq!(normalize_download_threads(u8::MAX), 32);
    }

    #[test]
    fn loaded_legacy_thread_values_are_normalized_before_use() {
        let mut config: AppConfig = serde_json::from_str(r#"{"download_threads":20}"#).unwrap();
        assert_eq!(config.download_threads, 20);
        config = config.normalized();
        assert_eq!(config.download_threads, 16);
    }

    #[test]
    fn legacy_advanced_mode_is_always_discarded() {
        let config: AppConfig =
            serde_json::from_str(r#"{"easy_mode_enabled":false,"enable_advanced_options":true}"#)
                .unwrap();
        let normalized = config.normalized();
        assert!(!normalized.enable_advanced_options);
    }

    #[test]
    fn supported_advanced_preferences_survive_while_legacy_diskpart_is_reset() {
        let config: AppConfig = serde_json::from_str(
            r#"{"install_prefs":{"run_diskpart_scripts":true,"advanced_options":{"disable_windows_defender":true}}}"#,
        )
        .unwrap();
        let normalized = config.normalized();
        assert!(!normalized.install_prefs.run_diskpart_scripts);
        assert!(
            normalized
                .install_prefs
                .advanced_options
                .disable_windows_defender
        );
    }

    #[test]
    fn stale_ui_snapshot_cannot_erase_a_newer_pe_catalogue() {
        let stale_ui = AppConfig {
            language: "en-US".to_owned(),
            ..Default::default()
        };

        let latest = AppConfig {
            pe_cache: crate::download::config::PeCache {
                pe_list: vec![crate::download::config::CachedPE {
                    display_name: "LetRecovery PE".to_owned(),
                    filename: "LetRecovery_PE.wim".to_owned(),
                    md5: Some("900150983CD24FB0D6963F7D28E17F72".to_owned()),
                    sha256: None,
                }],
                version: 1,
            },
            ..Default::default()
        };

        let merged = stale_ui.merge_latest_pe_cache(latest);
        assert_eq!(merged.language, "en-US");
        assert_eq!(merged.pe_cache.pe_list.len(), 1);
        assert_eq!(merged.pe_cache.pe_list[0].filename, "LetRecovery_PE.wim");
    }
}
