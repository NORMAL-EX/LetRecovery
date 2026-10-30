//! Compile-time branding for customised builds (the LetRecovery customisation service).
//!
//! Official builds set nothing and get exactly the defaults below. The customisation pipeline
//! exports `LR_BRAND_*` environment variables before `cargo build`; `option_env!` registers them
//! as build inputs, so Cargo rebuilds whenever one changes. The desktop and the PE executable
//! read the same values from here, which keeps the staging directory, WIM and PE executable
//! names identical on both sides of the hand-off.

pub const OFFICIAL_PRODUCT_NAME: &str = "LetRecovery";
/// Kept verbatim in rebranded text: the customisation notice names the original team.
const PROTECTED_TEAM_NAME: &str = "LetRecovery Team";

const fn text(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(value) if !value.is_empty() => value,
        _ => default,
    }
}

const fn flag(value: Option<&'static str>, default: bool) -> bool {
    match value {
        Some(value) => match value.as_bytes() {
            b"1" | b"true" => true,
            b"0" | b"false" => false,
            _ => default,
        },
        None => default,
    }
}

/// Licence tier of a build. Official builds are the project's own releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edition {
    Official,
    /// Free key for non-profit organisations; About shows the friendship notice.
    Nonprofit,
    /// Personal use only; About and a startup notice forbid commercial use and resale.
    Personal,
    /// Commercial licence: resale allowed, optional usage password and promotions.
    Commercial,
}

const fn edition(value: Option<&'static str>) -> Edition {
    match value {
        Some(value) => match value.as_bytes() {
            b"nonprofit" => Edition::Nonprofit,
            b"personal" => Edition::Personal,
            b"commercial" => Edition::Commercial,
            _ => Edition::Official,
        },
        None => Edition::Official,
    }
}

pub const EDITION: Edition = edition(option_env!("LR_BRAND_EDITION"));
pub const PRODUCT_NAME: &str = text(option_env!("LR_BRAND_PRODUCT_NAME"), OFFICIAL_PRODUCT_NAME);
pub const COPYRIGHT: &str = text(
    option_env!("LR_BRAND_COPYRIGHT"),
    "© 2026-present Cloud-PE Dev. (NORMAL-EX & hwyyds)",
);
/// Display version; `None` keeps the build-date version produced by `build.rs`.
pub const VERSION: Option<&str> = match option_env!("LR_BRAND_VERSION") {
    Some(value) if !value.is_empty() => Some(value),
    _ => None,
};
pub const FILE_DESCRIPTION: &str = text(
    option_env!("LR_BRAND_FILE_DESCRIPTION"),
    "Windows系统一键重装工具",
);
pub const PE_WIM_NAME: &str = text(option_env!("LR_BRAND_PE_WIM_NAME"), "LetRecovery_PE.wim");
pub const PE_DIR_NAME: &str = text(option_env!("LR_BRAND_PE_DIR_NAME"), "LetRecovery_PE");
pub const PE_EXE_NAME: &str = text(option_env!("LR_BRAND_PE_EXE_NAME"), "LetRecoveryPE.exe");
/// Folder of the PE program inside the WIM (`X:\Program Files\<dir>\<exe>`).
pub const PE_PROGRAM_DIR: &str = text(option_env!("LR_BRAND_PE_PROGRAM_DIR"), "LetRecoveryPE");
pub const CATALOG_URL: &str = text(
    option_env!("LR_BRAND_CATALOG_URL"),
    "https://letrecovery.cloud-pe.cn/v4/",
);
pub const FEEDBACK_URL: &str = text(
    option_env!("LR_BRAND_FEEDBACK_URL"),
    "https://api.cloud-pe.cn/v1/feedback",
);
pub const INSTALL_STATS_URL: &str = text(
    option_env!("LR_BRAND_INSTALL_STATS_URL"),
    "https://api.cloud-pe.cn/v1/install",
);
pub const HOMEPAGE_URL: &str = text(
    option_env!("LR_BRAND_HOMEPAGE_URL"),
    "https://letrecovery.net",
);
pub const ISSUES_URL: &str = text(
    option_env!("LR_BRAND_ISSUES_URL"),
    "https://github.com/NORMAL-EX/LetRecovery/issues",
);
pub const LICENSE_URL: &str = text(
    option_env!("LR_BRAND_LICENSE_URL"),
    "https://github.com/NORMAL-EX/LetRecovery/blob/main/LICENSE",
);
/// Commercial usage password: lower-case hex SHA-256 of `salt + password`; empty = none.
pub const PASSWORD_SHA256: &str = text(option_env!("LR_BRAND_PASSWORD_SHA256"), "");
pub const PASSWORD_SALT: &str = text(option_env!("LR_BRAND_PASSWORD_SALT"), "");
/// Commercial promotions, one `name<TAB>https-url` pair per line.
pub const PROMOTIONS: &str = text(option_env!("LR_BRAND_PROMOTIONS"), "");

/// Feature switches. Everything defaults to the official behaviour.
pub mod feature {
    use super::flag;

    /// Online catalogue (system images, software, easy-mode systems, online PE).
    pub const ONLINE_CATALOG: bool = flag(option_env!("LR_BRAND_FEATURE_ONLINE_CATALOG"), true);
    pub const SYSTEM_DOWNLOAD: bool =
        ONLINE_CATALOG && flag(option_env!("LR_BRAND_FEATURE_SYSTEM_DOWNLOAD"), true);
    pub const SOFTWARE_DOWNLOAD: bool =
        ONLINE_CATALOG && flag(option_env!("LR_BRAND_FEATURE_SOFTWARE_DOWNLOAD"), true);
    pub const PREINSTALL_SOFTWARE: bool =
        ONLINE_CATALOG && flag(option_env!("LR_BRAND_FEATURE_PREINSTALL_SOFTWARE"), true);
    pub const EASY_MODE: bool = flag(option_env!("LR_BRAND_FEATURE_EASY_MODE"), true);
    pub const AUTOMATION_EXPORT: bool =
        flag(option_env!("LR_BRAND_FEATURE_AUTOMATION_EXPORT"), true);
    pub const AUTOMATIC_FEEDBACK: bool =
        flag(option_env!("LR_BRAND_FEATURE_AUTOMATIC_FEEDBACK"), true);
    pub const INSTALL_STATS: bool = flag(option_env!("LR_BRAND_FEATURE_INSTALL_STATS"), true);
    pub const BACKUP: bool = flag(option_env!("LR_BRAND_FEATURE_BACKUP"), true);
    pub const TOOLS: bool = flag(option_env!("LR_BRAND_FEATURE_TOOLS"), true);
    pub const HARDWARE_INFO: bool = flag(option_env!("LR_BRAND_FEATURE_HARDWARE_INFO"), true);
}

pub const fn is_customized() -> bool {
    !matches!(EDITION, Edition::Official)
}

/// Replaces the official product name in user-visible text of a customised build.
pub fn rebrand(text: String) -> String {
    if PRODUCT_NAME == OFFICIAL_PRODUCT_NAME || !text.contains(OFFICIAL_PRODUCT_NAME) {
        return text;
    }
    const GUARD: &str = "\u{1}LR-TEAM\u{1}";
    text.replace(PROTECTED_TEAM_NAME, GUARD)
        .replace(OFFICIAL_PRODUCT_NAME, PRODUCT_NAME)
        .replace(GUARD, PROTECTED_TEAM_NAME)
}

pub fn password_required() -> bool {
    matches!(EDITION, Edition::Commercial) && PASSWORD_SHA256.len() == 64
}

/// Constant-time comparison of `sha256(salt + input)` with the configured hash.
pub fn password_matches(input: &str) -> bool {
    use sha2::{Digest, Sha256};
    if !password_required() {
        return true;
    }
    let digest = Sha256::digest(format!("{PASSWORD_SALT}{input}").as_bytes());
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let expected = PASSWORD_SHA256.to_ascii_lowercase();
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
}

/// Commercial promotions shown on the About page (at most three, HTTPS only).
pub fn promotions() -> Vec<(String, String)> {
    if !matches!(EDITION, Edition::Commercial) {
        return Vec::new();
    }
    PROMOTIONS
        .lines()
        .filter_map(|line| {
            let (name, url) = line.split_once('\t')?;
            let (name, url) = (name.trim(), url.trim());
            (!name.is_empty() && url.starts_with("https://"))
                .then(|| (name.to_owned(), url.to_owned()))
        })
        .take(3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers_accept_only_explicit_values() {
        assert!(flag(Some("1"), false));
        assert!(!flag(Some("0"), true));
        assert!(flag(Some("maybe"), true));
        assert!(!flag(None, false));
        assert_eq!(edition(Some("personal")), Edition::Personal);
        assert_eq!(edition(Some("commercial")), Edition::Commercial);
        assert_eq!(edition(Some("anything")), Edition::Official);
        assert_eq!(text(Some(""), "x"), "x");
    }

    #[test]
    fn official_build_keeps_text_and_names() {
        if !is_customized() {
            assert_eq!(
                rebrand("LetRecovery 正在运行".to_owned()),
                "LetRecovery 正在运行"
            );
            assert_eq!(PE_DIR_NAME, "LetRecovery_PE");
            assert_eq!(PE_WIM_NAME, "LetRecovery_PE.wim");
            assert!(password_matches("anything"));
        }
    }
}
