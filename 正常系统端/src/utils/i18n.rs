//! 国际化（i18n）模块
//!
//! 提供多语言支持，包括：
//! - 从 `{软件运行目录}/lang` 目录加载语言文件
//! - 支持运行时切换语言
//! - 语言设置持久化到配置文件
//! - 高性能翻译查找

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::path::get_exe_dir;

/// 语言文件结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageFile {
    /// 语言显示名称（如 "English (United States)"）
    pub language: String,
    /// 翻译作者
    pub author: String,
    /// 翻译数据映射（原文 -> 译文）
    pub data: HashMap<String, String>,
}

/// 可用语言信息
#[derive(Debug, Clone)]
pub struct LanguageInfo {
    /// 语言代码（如 "en-US"，来自文件名）
    pub code: String,
    /// 语言显示名称
    pub display_name: String,
    /// 翻译作者
    pub author: String,
}

/// 全局翻译管理器
struct I18nManager {
    /// 当前语言代码
    current_language: String,
    /// 当前翻译表
    translations: HashMap<String, String>,
    /// 可用语言列表缓存
    available_languages: Vec<LanguageInfo>,
}

impl I18nManager {
    fn new() -> Self {
        Self {
            current_language: String::from("zh-CN"),
            translations: HashMap::new(),
            available_languages: Vec::new(),
        }
    }
}

/// 全局翻译管理器实例
static I18N_MANAGER: OnceLock<RwLock<I18nManager>> = OnceLock::new();

/// 获取语言文件目录路径
pub fn get_lang_dir() -> PathBuf {
    get_exe_dir().join("lang")
}

/// 初始化国际化系统
///
/// # Arguments
/// * `language_code` - 要加载的语言代码（如 "zh-CN", "en-US"）
///                     如果为 "zh-CN" 或空，则使用内置的简体中文
pub fn init(language_code: &str) {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let mut guard = manager.write();

    // 刷新可用语言列表
    guard.available_languages = scan_available_languages();

    // 加载指定语言
    load_language_internal(&mut guard, language_code);
}

/// 内部加载语言函数
fn load_language_internal(manager: &mut I18nManager, language_code: &str) {
    // 简体中文使用空翻译表（直接显示原文）
    if language_code.is_empty() || language_code == "zh-CN" {
        manager.current_language = String::from("zh-CN");
        manager.translations.clear();
        log::info!("语言设置为简体中文（内置）");
        return;
    }

    // 尝试加载语言文件
    let lang_dir = get_lang_dir();
    let lang_file = lang_dir.join(format!("{}.json", language_code));

    if !lang_file.exists() {
        log::warn!("语言文件不存在: {}，使用简体中文", lang_file.display());
        manager.current_language = String::from("zh-CN");
        manager.translations.clear();
        return;
    }

    match std::fs::read_to_string(&lang_file) {
        Ok(content) => match serde_json::from_str::<LanguageFile>(&content) {
            Ok(lang_data) => {
                manager.current_language = language_code.to_string();
                manager.translations = lang_data.data;
                log::info!(
                    "已加载语言: {} ({}) - 作者: {}",
                    lang_data.language,
                    language_code,
                    lang_data.author
                );
            }
            Err(e) => {
                log::warn!("解析语言文件失败: {} - {}，使用简体中文", lang_file.display(), e);
                manager.current_language = String::from("zh-CN");
                manager.translations.clear();
            }
        },
        Err(e) => {
            log::warn!("读取语言文件失败: {} - {}，使用简体中文", lang_file.display(), e);
            manager.current_language = String::from("zh-CN");
            manager.translations.clear();
        }
    }
}

/// 切换语言
///
/// # Arguments
/// * `language_code` - 目标语言代码
pub fn switch_language(language_code: &str) {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let mut guard = manager.write();
    load_language_internal(&mut guard, language_code);
}

/// 获取当前语言代码
pub fn current_language() -> String {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let guard = manager.read();
    guard.current_language.clone()
}

/// 翻译字符串
///
/// 如果当前语言有对应翻译，返回翻译后的字符串；
/// 否则返回原字符串。
///
/// # Arguments
/// * `text` - 要翻译的原文
///
/// # Returns
/// 翻译后的字符串，或原字符串
pub fn translate(text: &str) -> String {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let guard = manager.read();

    // 如果是简体中文或没有翻译表，直接返回原文
    if guard.current_language == "zh-CN" || guard.translations.is_empty() {
        return text.to_string();
    }

    // 查找翻译
    guard
        .translations
        .get(text)
        .cloned()
        .unwrap_or_else(|| text.to_string())
}

/// 扫描可用语言
///
/// 扫描 lang 目录下的所有有效语言文件
pub fn scan_available_languages() -> Vec<LanguageInfo> {
    let mut languages = Vec::new();

    // 始终添加简体中文作为内置语言
    languages.push(LanguageInfo {
        code: String::from("zh-CN"),
        display_name: String::from("简体中文 - 中华人民共和国"),
        author: String::from("内置"),
    });

    let lang_dir = get_lang_dir();
    if !lang_dir.exists() {
        return languages;
    }

    // 读取目录中的所有json文件
    let entries = match std::fs::read_dir(&lang_dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("无法读取语言目录: {} - {}", lang_dir.display(), e);
            return languages;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // 只处理json文件
        if path.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }

        // 从文件名提取语言代码
        let code = match path.file_stem().and_then(|s| s.to_str()) {
            Some(c) => c.to_string(),
            None => continue,
        };

        // 跳过zh-CN（已经作为内置语言添加）
        if code == "zh-CN" {
            continue;
        }

        // 尝试读取并解析语言文件
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<LanguageFile>(&content) {
                Ok(lang_data) => {
                    languages.push(LanguageInfo {
                        code,
                        display_name: lang_data.language,
                        author: lang_data.author,
                    });
                }
                Err(e) => {
                    log::debug!("解析语言文件失败: {} - {}", path.display(), e);
                }
            },
            Err(e) => {
                log::debug!("读取语言文件失败: {} - {}", path.display(), e);
            }
        }
    }

    // 按显示名称排序（简体中文保持在首位）
    languages[1..].sort_by(|a, b| a.display_name.cmp(&b.display_name));

    languages
}

/// 获取可用语言列表
///
/// 返回缓存的语言列表，如果需要刷新请调用 `refresh_available_languages()`
pub fn get_available_languages() -> Vec<LanguageInfo> {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let guard = manager.read();
    guard.available_languages.clone()
}

/// 刷新可用语言列表
pub fn refresh_available_languages() {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let mut guard = manager.write();
    guard.available_languages = scan_available_languages();
}

/// 翻译宏
///
/// 用于在代码中方便地进行文本翻译。
///
/// # Examples
/// ```
/// let text = tr!("你好");
/// let formatted = tr!("欢迎使用 {}", "LetRecovery");
/// ```
#[macro_export]
macro_rules! tr {
    // 简单翻译
    ($text:expr) => {
        $crate::utils::i18n::translate($text)
    };
    // 带格式化参数的翻译
    ($text:expr, $($arg:tt)*) => {{
        let translated = $crate::utils::i18n::translate($text);
        format!($($arg)*)
            .split("{}")
            .enumerate()
            .fold(translated, |acc, (i, _)| {
                acc.replacen("{}", &format!("{{{}}}", i), 1)
            })
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_no_translation() {
        init("zh-CN");
        assert_eq!(translate("测试文本"), "测试文本");
    }

    #[test]
    fn test_default_language() {
        init("");
        assert_eq!(current_language(), "zh-CN");
    }
}
