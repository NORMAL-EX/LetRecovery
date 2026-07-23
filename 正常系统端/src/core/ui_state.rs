use lr_core::boot_pca::BootPcaMode;
use serde::{Deserialize, Serialize};

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
/// Runtime-only Wi-Fi material intentionally remains skipped as in the legacy UI model.
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
    pub username: String,
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
    #[serde(default)]
    pub advanced_options: AdvancedOptionsData,
}

const fn default_true() -> bool {
    true
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
            advanced_options: AdvancedOptionsData::default(),
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
    }
}
