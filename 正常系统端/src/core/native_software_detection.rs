//! Read-only installed-software detection for the system-install application picker.
//!
//! The live system is queried through HKLM. An offline target is inspected by temporarily loading
//! only its SOFTWARE hive through the shared Win32 registry boundary. Detection is advisory: the
//! caller may use the result for defaults, but a missing/corrupt hive must never block Windows
//! installation.

use anyhow::{Context, Result};
use lr_core::registry::OfflineRegistry;
use lr_core::software_install::{validate_selected_packages, SelectedSoftwarePackage};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::download::config::SoftwareCategory;

static OFFLINE_HIVE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct LoadedSoftwareHive {
    alias: String,
}

impl LoadedSoftwareHive {
    fn load(path: &std::path::Path) -> Result<Self> {
        let alias = format!(
            "lr-software-{}-{}",
            std::process::id(),
            OFFLINE_HIVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        OfflineRegistry::load_hive(
            &alias,
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("offline SOFTWARE hive path is not Unicode"))?,
        )
        .with_context(|| format!("load offline SOFTWARE hive {}", path.display()))?;
        Ok(Self { alias })
    }

    fn root(&self) -> String {
        format!("HKLM\\{}", self.alias)
    }

    fn unload(mut self) -> Result<()> {
        let alias = std::mem::take(&mut self.alias);
        OfflineRegistry::unload_hive(&alias)
            .with_context(|| format!("unload offline SOFTWARE hive {alias}"))
    }
}

impl Drop for LoadedSoftwareHive {
    fn drop(&mut self) {
        if !self.alias.is_empty() {
            if let Err(error) = OfflineRegistry::unload_hive(&self.alias) {
                log::error!(
                    "[SOFTWARE DETECTION] emergency unload of {} failed: {error:#}",
                    self.alias
                );
            }
        }
    }
}

/// Return machine-wide uninstall display names from the Windows installation on `target`.
pub fn detect_installed_display_names(target: &str) -> Result<Vec<String>> {
    let letter = normalized_drive_letter(target)?;
    let current = lr_core::windows_storage::current_windows_drive_letter()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if letter == current {
        return collect_uninstall_names("HKLM");
    }

    let hive_path =
        std::path::PathBuf::from(format!("{letter}:\\Windows\\System32\\config\\SOFTWARE"));
    if !hive_path.exists() {
        return Ok(Vec::new());
    }
    let hive = LoadedSoftwareHive::load(&hive_path)?;
    let result = collect_uninstall_names(&hive.root());
    let unload = hive.unload();
    match (result, unload) {
        (Ok(names), Ok(())) => Ok(names),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(read_error), Err(unload_error)) => Err(anyhow::anyhow!(
            "read offline uninstall inventory failed: {read_error:#}; additionally {unload_error:#}"
        )),
    }
}

/// Intersect a server-authorized catalogue with detected uninstall names.
pub fn default_packages_for_installed_names(
    categories: &[SoftwareCategory],
    installed_names: &[String],
) -> Result<Vec<SelectedSoftwarePackage>> {
    let mut selected = Vec::new();
    let mut ids = BTreeSet::new();
    for software in categories
        .iter()
        .flat_map(|category| category.items.iter())
        .filter(|software| !software.vm_tools)
    {
        if !installed_names
            .iter()
            .any(|installed| software_names_match(&software.name, installed))
        {
            continue;
        }
        let id = software.id.trim().to_ascii_lowercase();
        if !ids.insert(id) {
            continue;
        }
        if let Some(package) =
            super::native_download_controller::NativeDownloadController::selected_package(software)
        {
            selected.push(package);
        }
    }
    validate_selected_packages(&selected)
        .context("detected software defaults failed catalogue validation")?;
    Ok(selected)
}

fn collect_uninstall_names(root: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    // Microsoft documents DisplayName under the machine-wide Uninstall key as the installed
    // product name. A 64-bit Windows installation has separate 64-bit and redirected 32-bit
    // registry views; an offline SOFTWARE hive mounted under a private alias cannot be opened via
    // KEY_WOW64_* as the normal HKLM\SOFTWARE root, so enumerate its on-disk WOW6432Node peer too.
    // RegLoadKey returns a Win32 status and the hive must be unloaded; LoadedSoftwareHive provides
    // that symmetric lifetime even when enumeration returns early.
    for suffix in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let path = format!("{root}\\{suffix}");
        if !OfflineRegistry::key_exists(&path)? {
            continue;
        }
        for child in OfflineRegistry::subkey_names(&path)? {
            let key = format!("{path}\\{child}");
            let Some(name) = OfflineRegistry::query_string_optional(&key, "DisplayName")? else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() || name.starts_with("KB") {
                continue;
            }
            let folded = name.to_lowercase();
            if seen.insert(folded) {
                names.push(name.to_owned());
            }
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    Ok(names)
}

fn normalized_drive_letter(value: &str) -> Result<char> {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() != 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        anyhow::bail!("target partition must be a drive letter, got {value:?}");
    }
    let letter = (bytes[0] as char).to_ascii_uppercase();
    if !('C'..='Z').contains(&letter) {
        anyhow::bail!("target partition is outside the supported C:-Z: range");
    }
    Ok(letter)
}

fn software_names_match(catalogue: &str, installed: &str) -> bool {
    let catalogue = normalized_name_tokens(catalogue);
    let installed = normalized_name_tokens(installed);
    if catalogue.is_empty() || installed.is_empty() {
        return false;
    }
    catalogue == installed
        || installed
            .strip_prefix(catalogue.as_slice())
            .is_some_and(|suffix| {
                !suffix.is_empty() && suffix.iter().all(|token| metadata_token(token))
            })
}

fn normalized_name_tokens(value: &str) -> Vec<String> {
    let mut normalized = String::new();
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            normalized.push(character);
        } else if !normalized.ends_with(' ') {
            normalized.push(' ');
        }
    }
    normalized
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn metadata_token(token: &str) -> bool {
    matches!(
        token,
        "x64" | "x86" | "amd64" | "win64" | "win32" | "64bit" | "32bit" | "version" | "build"
    ) || token.chars().all(|character| character.is_ascii_digit())
        || token.strip_prefix('v').is_some_and(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::config::OnlineSoftware;

    fn software(id: &str, name: &str) -> OnlineSoftware {
        OnlineSoftware {
            id: id.to_owned(),
            name: name.to_owned(),
            description: String::new(),
            update_date: String::new(),
            file_size: String::new(),
            version: None,
            icon_url: None,
            download_url: format!("https://example.test/{id}.exe"),
            download_url_x86: None,
            download_url_nt5: None,
            filename: format!("{id}.exe"),
            silent_command: Some("\"{installer}\" /S".to_owned()),
            requires_admin: false,
            vm_tools: false,
            md5: None,
            sha256: None,
            md5_x86: None,
            sha256_x86: None,
            md5_nt5: None,
            sha256_nt5: None,
        }
    }

    #[test]
    fn conservative_name_matching_accepts_only_version_and_architecture_suffixes() {
        assert!(software_names_match("7-Zip", "7-Zip 26.00 (x64)"));
        assert!(software_names_match("Bandizip", "Bandizip v7.40"));
        assert!(software_names_match("ToDesk", "ToDesk"));
        assert!(!software_names_match("Zip", "7-Zip 26.00 (x64)"));
        assert!(!software_names_match("ToDesk", "ToDesk Helper"));
        assert!(!software_names_match("Visual Studio", "Visual Studio Code"));
    }

    #[test]
    fn catalogue_intersection_returns_stable_selected_packages_only() {
        let categories = vec![SoftwareCategory {
            id: "tools".to_owned(),
            name: "Tools".to_owned(),
            description: String::new(),
            items: vec![
                software("7zip-x64", "7-Zip"),
                software("bandizip-x64", "Bandizip"),
                software("todesk", "ToDesk"),
            ],
        }];
        let selected = default_packages_for_installed_names(
            &categories,
            &["7-Zip 26.00 (x64)".to_owned(), "ToDesk Helper".to_owned()],
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "7zip-x64");
    }

    #[test]
    fn drive_letter_validation_rejects_paths_and_unsupported_letters() {
        assert_eq!(normalized_drive_letter("c:").unwrap(), 'C');
        assert!(normalized_drive_letter(r"C:\Windows").is_err());
        assert!(normalized_drive_letter("A:").is_err());
    }
}
