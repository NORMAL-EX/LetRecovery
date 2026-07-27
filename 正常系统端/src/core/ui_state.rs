use lr_core::boot_pca::BootPcaMode;
use lr_core::unattend_account::BuiltInAdministratorOptions;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use windows::core::PWSTR;
#[cfg(windows)]
use windows::Win32::System::WindowsProgramming::GetUserNameW;
#[cfg(windows)]
use winreg::enums::HKEY_LOCAL_MACHINE;
#[cfg(windows)]
use winreg::RegKey;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub enum BootModeSelection {
    #[default]
    Auto,
    UEFI,
    Legacy,
}

impl BootModeSelection {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::UEFI => 1,
            Self::Legacy => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum DriverAction {
    None,
    SaveOnly,
    #[default]
    AutoImport,
}

/// Serializable installation options shared by the native UI, config file and CLI.
/// Runtime-only Wi-Fi material and the current-session username are deliberately skipped.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedOptionsData {
    pub remove_shortcut_arrow: bool,
    pub restore_classic_context_menu: bool,
    pub bypass_nro: bool,
    pub disable_windows_update: bool,
    pub disable_windows_defender: bool,
    pub disable_reserved_storage: bool,
    pub disable_uac: bool,
    pub disable_device_encryption: bool,
    pub remove_uwp_apps: bool,
    pub migrate_wifi: bool,
    #[serde(skip)]
    pub wifi_profile_xml: String,
    #[serde(skip)]
    pub wifi_ssid: String,
    #[serde(skip)]
    pub wifi_detected: Option<bool>,
    pub run_script_during_deploy: bool,
    pub deploy_script_path: String,
    pub run_script_first_login: bool,
    pub first_login_script_path: String,
    pub import_custom_drivers: bool,
    pub custom_drivers_path: String,
    pub import_storage_controller_drivers: bool,
    pub import_registry_file: bool,
    pub registry_file_path: String,
    pub import_custom_files: bool,
    pub custom_files_path: String,
    pub custom_username: bool,
    /// Current-install-session value. It is populated from Windows at runtime and never persisted
    /// to config.json; conversion into `AdvancedOptions` still carries it into the PE handoff.
    #[serde(skip)]
    pub username: String,
    pub builtin_administrator: BuiltInAdministratorOptions,
    pub custom_volume_label: bool,
    pub volume_label: String,
    pub win7_inject_usb3_driver: bool,
    pub win7_usb3_driver_path: String,
    pub win7_inject_nvme_driver: bool,
    pub win7_nvme_driver_path: String,
    pub win7_fix_acpi_bsod: bool,
    pub win7_fix_storage_bsod: bool,
    pub win7_uefi_patch: bool,
    pub xp_inject_usb3_driver: bool,
    pub xp_inject_nvme_driver: bool,
    #[serde(skip)]
    pub xp_defaults_applied: bool,
}

impl AdvancedOptionsData {
    /// Restores non-persistent defaults after config deserialization.
    ///
    /// Old configs can contain neither account mode or an empty volume label. The native page is
    /// a two-choice radio group, so the ordinary current-user path is the deterministic default.
    pub fn apply_runtime_defaults(&mut self) {
        if self.username.trim().is_empty() {
            self.username = default_install_username();
        }
        if self.volume_label.trim().is_empty() {
            self.volume_label = "OS".to_string();
        }
        if self.builtin_administrator.account_name.trim().is_empty() {
            self.builtin_administrator.account_name = "Administrator".to_string();
        }
        self.custom_username = !self.builtin_administrator.enabled;
    }
}

/// Returns a safe default name for the ordinary local account created by Windows Setup.
///
/// The current token username wins. If USER32/Advapi cannot provide it, use a concise system
/// manufacturer token such as ASUS or VMware, and finally the stable `User` fallback.
pub(crate) fn default_install_username() -> String {
    windows_login_username()
        .or_else(system_manufacturer_username)
        .unwrap_or_else(|| "User".to_string())
}

#[cfg(windows)]
fn windows_login_username() -> Option<String> {
    const USERNAME_CAPACITY: usize = 257;
    let mut buffer = [0_u16; USERNAME_CAPACITY];
    let mut length = buffer.len() as u32;
    unsafe { GetUserNameW(PWSTR(buffer.as_mut_ptr()), &mut length) }.ok()?;
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(length as usize)
        .min(buffer.len());
    let username = String::from_utf16(&buffer[..length]).ok()?;
    (!username.trim().is_empty()).then(|| username.trim().to_string())
}

#[cfg(not(windows))]
fn windows_login_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|username| !username.trim().is_empty())
}

#[cfg(windows)]
fn system_manufacturer_username() -> Option<String> {
    let bios = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"HARDWARE\DESCRIPTION\System\BIOS")
        .ok()?;
    let manufacturer: String = bios.get_value("SystemManufacturer").ok()?;
    manufacturer_account_candidate(&manufacturer)
}

#[cfg(not(windows))]
fn system_manufacturer_username() -> Option<String> {
    None
}

fn manufacturer_account_candidate(manufacturer: &str) -> Option<String> {
    let trimmed = manufacturer.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_ascii_lowercase();
    if [
        "to be filled by o.e.m.",
        "default string",
        "system manufacturer",
        "unknown",
        "none",
    ]
    .iter()
    .any(|placeholder| normalized == *placeholder)
    {
        return None;
    }

    let candidate = trimmed
        .split(|character: char| {
            character.is_whitespace() || r#""/\[]:;|=,+*?<>@"#.contains(character)
        })
        .find(|part| !part.is_empty())?;
    let mut result = String::new();
    let mut utf16_units = 0;
    for character in candidate.chars() {
        if !(character.is_alphanumeric() || matches!(character, '-' | '_' | '.')) {
            continue;
        }
        let units = character.len_utf16();
        if utf16_units + units > 20 {
            break;
        }
        result.push(character);
        utf16_units += units;
    }
    while result.ends_with('.') {
        result.pop();
    }
    (!result.is_empty()).then_some(result)
}

impl From<&AdvancedOptionsData> for super::advanced_options::AdvancedOptions {
    fn from(value: &AdvancedOptionsData) -> Self {
        Self {
            remove_shortcut_arrow: value.remove_shortcut_arrow,
            restore_classic_context_menu: value.restore_classic_context_menu,
            bypass_nro: value.bypass_nro,
            disable_windows_update: value.disable_windows_update,
            disable_windows_defender: value.disable_windows_defender,
            disable_reserved_storage: value.disable_reserved_storage,
            disable_uac: value.disable_uac,
            disable_device_encryption: value.disable_device_encryption,
            remove_uwp_apps: value.remove_uwp_apps,
            migrate_wifi: value.migrate_wifi,
            wifi_profile_xml: value.wifi_profile_xml.clone(),
            wifi_ssid: value.wifi_ssid.clone(),
            wifi_detected: value.wifi_detected,
            run_script_during_deploy: value.run_script_during_deploy,
            deploy_script_path: value.deploy_script_path.clone(),
            run_script_first_login: value.run_script_first_login,
            first_login_script_path: value.first_login_script_path.clone(),
            import_custom_drivers: value.import_custom_drivers,
            custom_drivers_path: value.custom_drivers_path.clone(),
            import_storage_controller_drivers: value.import_storage_controller_drivers,
            import_registry_file: value.import_registry_file,
            registry_file_path: value.registry_file_path.clone(),
            import_custom_files: value.import_custom_files,
            custom_files_path: value.custom_files_path.clone(),
            custom_username: value.custom_username,
            username: value.username.clone(),
            builtin_administrator: value.builtin_administrator.clone(),
            custom_volume_label: value.custom_volume_label,
            volume_label: value.volume_label.clone(),
            win7_inject_usb3_driver: value.win7_inject_usb3_driver,
            win7_usb3_driver_path: value.win7_usb3_driver_path.clone(),
            win7_inject_nvme_driver: value.win7_inject_nvme_driver,
            win7_nvme_driver_path: value.win7_nvme_driver_path.clone(),
            win7_fix_acpi_bsod: value.win7_fix_acpi_bsod,
            win7_fix_storage_bsod: value.win7_fix_storage_bsod,
            win7_uefi_patch: value.win7_uefi_patch,
            xp_inject_usb3_driver: value.xp_inject_usb3_driver,
            xp_inject_nvme_driver: value.xp_inject_nvme_driver,
            xp_defaults_applied: value.xp_defaults_applied,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPrefs {
    #[serde(default = "default_true")]
    pub format_partition: bool,
    #[serde(default = "default_true")]
    pub repair_boot: bool,
    #[serde(default = "default_true")]
    pub unattended_install: bool,
    #[serde(default = "default_true")]
    pub export_drivers: bool,
    #[serde(default = "default_true")]
    pub auto_reboot: bool,
    #[serde(default)]
    pub run_diskpart_scripts: bool,
    #[serde(default)]
    pub boot_mode: BootModeSelection,
    #[serde(default)]
    pub boot_pca_mode: BootPcaMode,
    #[serde(default)]
    pub driver_action: DriverAction,
    #[serde(default = "default_advanced_options_data")]
    pub advanced_options: AdvancedOptionsData,
}

const fn default_true() -> bool {
    true
}

fn default_advanced_options_data() -> AdvancedOptionsData {
    let mut options = AdvancedOptionsData::default();
    options.apply_runtime_defaults();
    options
}

impl Default for InstallPrefs {
    fn default() -> Self {
        Self {
            format_partition: true,
            repair_boot: true,
            unattended_install: true,
            export_drivers: true,
            auto_reboot: true,
            run_diskpart_scripts: false,
            boot_mode: BootModeSelection::Auto,
            boot_pca_mode: BootPcaMode::Auto,
            driver_action: DriverAction::AutoImport,
            advanced_options: default_advanced_options_data(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_partial_install_preferences_keep_existing_defaults() {
        let prefs: InstallPrefs = serde_json::from_str("{}").unwrap();
        assert!(prefs.format_partition);
        assert!(prefs.repair_boot);
        assert!(prefs.unattended_install);
        assert!(prefs.auto_reboot);
        assert_eq!(prefs.driver_action, DriverAction::AutoImport);
        assert!(prefs.advanced_options.custom_username);
        assert!(!prefs.advanced_options.username.is_empty());
        assert_eq!(prefs.advanced_options.volume_label, "OS");
    }

    #[test]
    fn runtime_username_is_never_persisted_to_config_json() {
        let mut data = AdvancedOptionsData::default();
        data.apply_runtime_defaults();
        data.username = "SensitiveCurrentUser".to_string();

        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("SensitiveCurrentUser"));
        assert!(!json.contains("\"username\""));

        let mut restored: AdvancedOptionsData = serde_json::from_str(&json).unwrap();
        restored.apply_runtime_defaults();
        assert!(!restored.username.is_empty());
    }

    #[test]
    fn manufacturer_fallback_produces_safe_concise_account_names() {
        assert_eq!(
            manufacturer_account_candidate("VMware, Inc.").as_deref(),
            Some("VMware")
        );
        assert_eq!(
            manufacturer_account_candidate("ASUS").as_deref(),
            Some("ASUS")
        );
        assert_eq!(
            manufacturer_account_candidate("To Be Filled By O.E.M."),
            None
        );
    }

    #[test]
    fn runtime_defaults_normalize_account_mode_and_volume_label() {
        let mut data = AdvancedOptionsData::default();
        data.apply_runtime_defaults();
        assert!(data.custom_username);
        assert!(!data.builtin_administrator.enabled);
        assert_eq!(data.volume_label, "OS");

        data.builtin_administrator.enabled = true;
        data.builtin_administrator.account_name.clear();
        data.apply_runtime_defaults();
        assert!(!data.custom_username);
        assert_eq!(data.builtin_administrator.account_name, "Administrator");
    }
}
