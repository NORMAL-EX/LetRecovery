//! Single compatibility bridge to the legacy APPX implementation.
//!
//! Only current-system PackageManager operations are exposed. The legacy offline directory
//! mutation branch is intentionally unreachable from native UI code.

use super::{appx_legacy_impl as appx, tool_types as types};

pub(crate) fn current_inventory() -> Vec<types::AppxPackageInfo> {
    appx::get_appx_packages("__CURRENT__")
}

pub(crate) fn remove_current(packages: &[String]) -> (usize, usize) {
    appx::remove_appx_packages("__CURRENT__", packages)
}

pub(crate) fn is_critical(package: &str) -> bool {
    appx::is_system_critical_appx(package)
}
