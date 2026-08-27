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

/// The PE image is an independently replaceable user-managed artifact. Keep release catalogs in
/// the executable so an EXE update can add languages and missing UI text without rewriting the
/// WIM's language resources. Files in `lang` remain optional user overrides.
const EMBEDDED_LANGUAGE_CATALOGS: [(&str, &str); 5] = [
    (
        "en-US",
        include_str!("../../../assets/release/lang/en-US.json"),
    ),
    (
        "ja-JP",
        include_str!("../../../assets/release/lang/ja-JP.json"),
    ),
    (
        "ko-KR",
        include_str!("../../../assets/release/lang/ko-KR.json"),
    ),
    (
        "fr-FR",
        include_str!("../../../assets/release/lang/fr-FR.json"),
    ),
    (
        "de-DE",
        include_str!("../../../assets/release/lang/de-DE.json"),
    ),
];

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

/// 全局翻译管理器
struct I18nManager {
    /// 当前语言代码
    current_language: String,
    /// 当前翻译表
    translations: HashMap<String, String>,
}

impl I18nManager {
    fn new() -> Self {
        Self {
            current_language: String::from("zh-CN"),
            translations: HashMap::new(),
        }
    }
}

/// 全局翻译管理器实例
static I18N_MANAGER: OnceLock<RwLock<I18nManager>> = OnceLock::new();

pub const DPRK_EASTER_EGG_LANGUAGE: &str = "ko-KP";
pub const DPRK_EASTER_EGG_SLOGAN: &str = "김정은장군 만세!";

pub fn is_dprk_easter_egg_language(language_code: &str) -> bool {
    language_code.eq_ignore_ascii_case(DPRK_EASTER_EGG_LANGUAGE)
}

/// 获取语言文件目录路径
pub fn get_lang_dir() -> PathBuf {
    get_exe_dir().join("lang")
}

/// 初始化国际化系统
///
/// # Arguments
/// * `language_code` - 要加载的语言代码（如 "zh-CN", "en-US"）
///   如果为 "zh-CN" 或空，则使用内置的简体中文
pub fn init(language_code: &str) {
    let manager = I18N_MANAGER.get_or_init(|| RwLock::new(I18nManager::new()));
    let mut guard = manager.write();

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

    if is_dprk_easter_egg_language(language_code) {
        manager.current_language = DPRK_EASTER_EGG_LANGUAGE.to_string();
        manager.translations.clear();
        log::info!("已启用朝鲜文彩蛋语言");
        return;
    }

    // PE must not depend on a separately injected language catalog.  Windows NLS supplies the
    // complete character conversion, while an optional file can override individual terms.
    if language_code.eq_ignore_ascii_case("zh-TW") {
        manager.current_language = String::from("zh-TW");
        manager.translations.clear();
        let lang_file = get_lang_dir().join("zh-TW.json");
        if let Ok(content) = std::fs::read_to_string(&lang_file) {
            match serde_json::from_str::<LanguageFile>(&content) {
                Ok(language) => manager.translations = language.data,
                Err(error) => log::warn!(
                    "解析繁體中文覆写文件失败: {} - {}，继续使用内置繁體中文",
                    lang_file.display(),
                    error
                ),
            }
        }
        log::info!("语言设置为繁體中文（内置 Windows NLS 转换）");
        return;
    }

    // 尝试加载语言文件
    let lang_dir = get_lang_dir();
    let lang_file = lang_dir.join(format!("{}.json", language_code));

    if !lang_file.exists() {
        if apply_language_fallback(manager, language_code) {
            log::warn!("语言文件不存在: {}，使用程序内置翻译", lang_file.display());
        } else {
            log::warn!("语言文件不存在: {}，使用简体中文", lang_file.display());
        }
        return;
    }

    match std::fs::read_to_string(&lang_file) {
        Ok(content) => match serde_json::from_str::<LanguageFile>(&content) {
            Ok(mut lang_data) => {
                merge_embedded_fallback(language_code, &mut lang_data);
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
                if apply_language_fallback(manager, language_code) {
                    log::warn!(
                        "解析语言文件失败: {} - {}，使用程序内置翻译",
                        lang_file.display(),
                        e
                    );
                } else {
                    log::warn!(
                        "解析语言文件失败: {} - {}，使用简体中文",
                        lang_file.display(),
                        e
                    );
                }
            }
        },
        Err(e) => {
            if apply_language_fallback(manager, language_code) {
                log::warn!(
                    "读取语言文件失败: {} - {}，使用程序内置翻译",
                    lang_file.display(),
                    e
                );
            } else {
                log::warn!(
                    "读取语言文件失败: {} - {}，使用简体中文",
                    lang_file.display(),
                    e
                );
            }
        }
    }
}

fn use_builtin_chinese(manager: &mut I18nManager) {
    manager.current_language = String::from("zh-CN");
    manager.translations.clear();
}

fn embedded_language(language_code: &str) -> Option<LanguageFile> {
    let (_, content) = EMBEDDED_LANGUAGE_CATALOGS
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(language_code))?;
    serde_json::from_str(content).ok()
}

fn load_embedded_language(manager: &mut I18nManager, language_code: &str) -> bool {
    let Some(language) = embedded_language(language_code) else {
        return false;
    };
    manager.current_language = language_code.to_string();
    manager.translations = language.data;
    true
}

/// 缺失、无法读取或无法解析外部语言文件时使用同一条确定性回退路径。
/// 返回 true 表示已使用受支持的内置语言，false 表示回到内置简体中文。
fn apply_language_fallback(manager: &mut I18nManager, language_code: &str) -> bool {
    if load_embedded_language(manager, language_code) {
        true
    } else {
        use_builtin_chinese(manager);
        false
    }
}

fn merge_embedded_fallback(language_code: &str, external: &mut LanguageFile) {
    let Some(embedded) = embedded_language(language_code) else {
        return;
    };

    // External language files remain user-overridable.  Only keys absent from an
    // older WIM copy are supplied by the executable bundled table.
    let mut merged = embedded.data;
    merged.extend(std::mem::take(&mut external.data));
    external.data = merged;
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

    translate_internal(&guard, text)
}

fn translate_internal(manager: &I18nManager, text: &str) -> String {
    if is_dprk_easter_egg_language(&manager.current_language) {
        return DPRK_EASTER_EGG_SLOGAN.to_string();
    }

    // 简体中文直接使用源字符串。
    if manager.current_language == "zh-CN" {
        return text.to_string();
    }

    if let Some(translated) = manager.translations.get(text) {
        return translated.clone();
    }

    if manager.current_language.eq_ignore_ascii_case("zh-TW") {
        return lr_core::traditional_chinese::to_traditional_chinese(text);
    }

    text.to_string()
}

/// 翻译并按顺序填充参数。
///
/// 先翻译模板 `text`，再把译文中出现的每个 `{}` 依次替换为 `args` 中的参数。
///
/// 由于 Rust 的 `format!` 要求格式串为编译期字面量，无法对运行期得到的译文直接格式化，
/// 因此这里采用顺序替换 `{}` 的方式实现参数插值。调用方需保证：
/// - 模板（即翻译表的 key）与译文中 `{}` 的数量、顺序一致，且与参数个数一致；
/// - 形如 `{:.1}`、`{:?}`、`{:x}` 等带格式说明的占位符，应在调用 `tr!` 之前
///   先用 `format!` 预格式化为普通字符串再作为参数传入（模板里统一写成 `{}`）。
///
/// 若参数不足以填满所有 `{}`，多余的占位符将原样保留。
pub fn translate_with_args(text: &str, args: &[String]) -> String {
    let translated = translate(text);
    let mut result = String::with_capacity(translated.len());
    let mut rest = translated.as_str();
    let mut iter = args.iter();

    while let Some(pos) = rest.find("{}") {
        result.push_str(&rest[..pos]);
        match iter.next() {
            Some(arg) => result.push_str(arg),
            None => result.push_str("{}"),
        }
        rest = &rest[pos + 2..];
    }
    result.push_str(rest);
    result
}

/// 翻译宏
///
/// 用于在代码中方便地进行文本翻译。
///
/// # Examples
/// ```
/// // 直接翻译字面量
/// let text = tr!("你好");
/// // 带参数：模板用 `{}` 占位，先翻译再按顺序填参
/// let formatted = tr!("欢迎使用 {}", "LetRecovery");
/// // 带格式说明的值需先预格式化为字符串再传入
/// let formatted_size = format!("{:.1}", 12.34_f64);
/// let size = tr!("已用 {} GB", formatted_size);
/// ```
#[macro_export]
macro_rules! tr {
    // 简单翻译
    ($text:expr) => {
        $crate::utils::i18n::translate($text)
    };
    // 带参数的翻译：先翻译模板，再按顺序把译文中的 `{}` 替换为各参数（参数需实现 Display）
    ($text:expr, $($arg:expr),+ $(,)?) => {
        $crate::utils::i18n::translate_with_args(
            $text,
            &[$(format!("{}", $arg)),+],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tr;

    fn placeholders(text: &str) -> Vec<String> {
        let mut placeholders = Vec::new();
        let mut remaining = text;
        while let Some(open) = remaining.find('{') {
            let after_open = &remaining[open..];
            let Some(close) = after_open.find('}') else {
                break;
            };
            placeholders.push(after_open[..=close].to_string());
            remaining = &after_open[close + 1..];
        }
        placeholders
    }

    #[test]
    fn test_translate_no_translation() {
        init("zh-CN");
        assert_eq!(translate("测试文本"), "测试文本");
    }

    #[test]
    fn test_default_language() {
        init("");
        let manager = I18N_MANAGER
            .get()
            .expect("i18n manager must be initialized");
        assert_eq!(manager.read().current_language, "zh-CN");
    }

    #[test]
    fn traditional_chinese_survives_without_an_external_pe_catalog() {
        assert_eq!(
            lr_core::traditional_chinese::to_traditional_chinese("系统安装与网络设置"),
            "系統安裝與網路設定"
        );
    }

    #[test]
    fn dprk_easter_egg_replaces_every_translated_label() {
        let mut manager = I18nManager::new();
        load_language_internal(&mut manager, DPRK_EASTER_EGG_LANGUAGE);
        assert_eq!(
            translate_internal(&manager, "系统安装"),
            DPRK_EASTER_EGG_SLOGAN
        );
        assert_eq!(
            translate_internal(&manager, "任意未收录文案"),
            DPRK_EASTER_EGG_SLOGAN
        );
    }

    #[test]
    fn test_translate_with_args_sequential() {
        init("zh-CN");
        // zh-CN 下译文即原文，验证占位符按顺序被替换
        assert_eq!(
            translate_with_args("已选择 {} 个分区", &["3".to_string()]),
            "已选择 3 个分区"
        );
        assert_eq!(
            translate_with_args("{} -> {}", &["A".to_string(), "B".to_string()]),
            "A -> B"
        );
    }

    #[test]
    fn test_translate_with_args_arity_mismatch() {
        init("zh-CN");
        // 参数不足时，多余占位符原样保留
        assert_eq!(
            translate_with_args("{} / {}", &["仅一个".to_string()]),
            "仅一个 / {}"
        );
        // 参数过多时，多余参数被忽略
        assert_eq!(
            translate_with_args("只有 {}", &["A".to_string(), "B".to_string()]),
            "只有 A"
        );
    }

    #[test]
    fn test_tr_macro_with_args() {
        init("zh-CN");
        assert_eq!(tr!("欢迎使用 {}", "LetRecovery"), "欢迎使用 LetRecovery");
        let formatted_size = format!("{:.1}", 12.34_f64);
        assert_eq!(tr!("已用 {} GB", formatted_size), "已用 12.3 GB");
    }

    #[test]
    fn embedded_release_catalogs_are_available_without_wim_language_updates() {
        let language = embedded_language("en-US").expect("embedded en-US must be valid JSON");
        assert_eq!(language.language, "English (United States)");
        assert_eq!(
            language.data.get("系统安装").map(String::as_str),
            Some("System Installation")
        );
        assert!(
            embedded_language("de-DE")
                .expect("embedded de-DE must be valid JSON")
                .data
                .contains_key("系统安装"),
            "embedded de-DE must cover the system installation label"
        );
    }

    #[test]
    fn embedded_release_catalogs_are_complete_and_preserve_placeholders() {
        let english = embedded_language("en-US").expect("embedded en-US must be valid JSON");

        for (code, _) in EMBEDDED_LANGUAGE_CATALOGS {
            let catalog = embedded_language(code)
                .unwrap_or_else(|| panic!("embedded {code} must be valid JSON"));
            assert_eq!(
                catalog.data.len(),
                english.data.len(),
                "{code} must contain the complete release key set"
            );
            for (key, english_value) in &english.data {
                let translated = catalog
                    .data
                    .get(key)
                    .unwrap_or_else(|| panic!("{code} is missing key {key}"));
                assert!(
                    !translated.trim().is_empty(),
                    "{code} has an empty key {key}"
                );
                assert_eq!(
                    placeholders(translated),
                    placeholders(english_value),
                    "{code} changed placeholders for key {key}"
                );
            }
        }
    }

    #[test]
    fn external_english_overrides_embedded_but_keeps_missing_keys() {
        let mut external = LanguageFile {
            language: "Custom English".to_string(),
            author: "User".to_string(),
            data: HashMap::from([("系统安装".to_string(), "Custom Install".to_string())]),
        };
        merge_embedded_fallback("en-US", &mut external);
        assert_eq!(
            external.data.get("系统安装").map(String::as_str),
            Some("Custom Install")
        );
        assert!(external.data.contains_key("系统备份"));
    }

    #[test]
    fn missing_or_invalid_english_uses_embedded_catalog() {
        let mut manager = I18nManager::new();
        assert!(apply_language_fallback(&mut manager, "en-US"));
        assert_eq!(manager.current_language, "en-US");
        assert_eq!(
            manager.translations.get("系统安装").map(String::as_str),
            Some("System Installation")
        );

        assert!(serde_json::from_str::<LanguageFile>("{ invalid json").is_err());
        assert!(apply_language_fallback(&mut manager, "en-US"));
        assert_eq!(manager.current_language, "en-US");
    }

    #[test]
    fn unsupported_language_fallback_and_merge_behavior_are_unchanged() {
        let mut manager = I18nManager::new();
        assert!(!apply_language_fallback(&mut manager, "es-ES"));
        assert_eq!(manager.current_language, "zh-CN");
        assert!(manager.translations.is_empty());

        let mut external = LanguageFile {
            language: "Español".to_string(),
            author: "User".to_string(),
            data: HashMap::from([("系统安装".to_string(), "Instalación".to_string())]),
        };
        merge_embedded_fallback("es-ES", &mut external);
        assert_eq!(external.data.len(), 1);
        assert_eq!(
            external.data.get("系统安装").map(String::as_str),
            Some("Instalación")
        );
        assert!(!external.data.contains_key("系统备份"));
    }
}
