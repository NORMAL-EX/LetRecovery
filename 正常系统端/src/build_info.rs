/// Compile-time marker for distributable test builds.
///
/// This is deliberately controlled by an explicit Cargo feature instead of the debug profile:
/// test packages still use release optimizations and the normal administrator manifest, while
/// production packages remain unmarked unless `dev-build` is intentionally enabled.
pub const DEV: bool = cfg!(feature = "dev-build");

const PRODUCTION_WINDOW_TITLE: &str = "LetRecovery - Windows系统一键重装工具";
const DEV_WINDOW_TITLE: &str = "LetRecovery 测试版 - 测试软件，仅供测试使用";
const PRODUCTION_ABOUT_TITLE: &str = "关于 LetRecovery";
const DEV_ABOUT_TITLE: &str = "关于 LetRecovery 测试版";
const PRODUCTION_PRODUCT_NAME: &str = "LetRecovery";
const DEV_PRODUCT_NAME: &str = "LetRecovery 测试版";
const PRODUCTION_DESCRIPTION: &str = "Windows 系统安装、备份和维护工具。";
const DEV_DESCRIPTION: &str = "测试软件，仅供测试使用。 Windows 系统安装、备份和维护工具。";

fn select(production: &'static str, dev: &'static str) -> &'static str {
    if DEV {
        dev
    } else {
        production
    }
}

pub fn window_title() -> String {
    crate::tr!(select(PRODUCTION_WINDOW_TITLE, DEV_WINDOW_TITLE))
}

pub fn about_title() -> String {
    crate::tr!(select(PRODUCTION_ABOUT_TITLE, DEV_ABOUT_TITLE))
}

pub fn product_name() -> String {
    crate::tr!(select(PRODUCTION_PRODUCT_NAME, DEV_PRODUCT_NAME))
}

pub fn description() -> String {
    crate::tr!(select(PRODUCTION_DESCRIPTION, DEV_DESCRIPTION))
}

pub fn display_version() -> String {
    if crate::utils::i18n::is_dprk_easter_egg_language(&crate::utils::i18n::current_language()) {
        crate::tr!(env!("BUILD_VERSION"))
    } else if DEV {
        crate::tr!("{}（测试版）", env!("BUILD_VERSION"))
    } else {
        env!("BUILD_VERSION").to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_marker_matches_the_cargo_feature() {
        assert_eq!(DEV, cfg!(feature = "dev-build"));
    }

    #[cfg(feature = "dev-build")]
    #[test]
    fn dev_sources_contain_all_required_test_markers() {
        assert!(select(PRODUCTION_WINDOW_TITLE, DEV_WINDOW_TITLE).contains("测试版"));
        assert!(select(PRODUCTION_WINDOW_TITLE, DEV_WINDOW_TITLE).contains("测试软件"));
        assert!(select(PRODUCTION_WINDOW_TITLE, DEV_WINDOW_TITLE).contains("仅供测试使用"));
        assert!(select(PRODUCTION_ABOUT_TITLE, DEV_ABOUT_TITLE).contains("测试版"));
        assert!(select(PRODUCTION_PRODUCT_NAME, DEV_PRODUCT_NAME).contains("测试版"));
        assert!(select(PRODUCTION_DESCRIPTION, DEV_DESCRIPTION).contains("测试软件"));
        assert!(select(PRODUCTION_DESCRIPTION, DEV_DESCRIPTION).contains("仅供测试使用"));
    }

    #[cfg(not(feature = "dev-build"))]
    #[test]
    fn production_sources_do_not_contain_test_markers() {
        for text in [
            select(PRODUCTION_WINDOW_TITLE, DEV_WINDOW_TITLE),
            select(PRODUCTION_ABOUT_TITLE, DEV_ABOUT_TITLE),
            select(PRODUCTION_PRODUCT_NAME, DEV_PRODUCT_NAME),
            select(PRODUCTION_DESCRIPTION, DEV_DESCRIPTION),
        ] {
            assert!(!text.contains("测试版"));
            assert!(!text.contains("测试软件"));
            assert!(!text.contains("仅供测试使用"));
        }
    }
}
