use lr_core::boot_pca::BootPcaMode;
use lr_core::software_install::SelectedSoftwarePackage;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedOptionsData {
    /// Current-install-only: preserve six local personal directories before deleting the old OS.
    #[serde(skip)]
    pub preserve_personal_files: bool,
    pub remove_shortcut_arrow: bool,
    pub restore_classic_context_menu: bool,
    pub bypass_nro: bool,
    pub disable_windows_update: bool,
    pub disable_windows_defender: bool,
    pub disable_reserved_storage: bool,
    pub disable_uac: bool,
    pub disable_device_encryption: bool,
    pub remove_uwp_apps: bool,
    /// Server-catalogue selections are valid only for the current process. They are deliberately
    /// not persisted because a later catalogue may reuse neither the URL nor the silent command.
    #[serde(skip)]
    pub preinstalled_software: Vec<SelectedSoftwarePackage>,
    /// User preference for the separate VMware Tools option. The runtime catalogue and positive
    /// VMware detection still gate visibility and execution, so a stale preference cannot install
    /// a server package on non-VMware hardware.
    pub install_vmware_tools: bool,
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

pub const ADVANCED_SYSTEM_OPTION_COUNT: usize = 10;

/// Target-version capability mask for installation advanced options.
///
/// The persisted preferences intentionally remain target-independent so switching images does not
/// destroy a user's choices.  The native page uses this mask for visibility, while the install
/// controller applies it again to its cloned intent so hidden options can never reach the offline
/// writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvancedOptionCapabilities {
    pub system_options: [bool; ADVANCED_SYSTEM_OPTION_COUNT],
    pub storage_controller_drivers: bool,
    pub windows_7: bool,
    pub xp: bool,
}

impl AdvancedOptionCapabilities {
    pub const fn unknown() -> Self {
        Self {
            system_options: [false; ADVANCED_SYSTEM_OPTION_COUNT],
            storage_controller_drivers: false,
            windows_7: false,
            xp: false,
        }
    }

    pub fn for_target(
        major_version: Option<u16>,
        minor_version: Option<u16>,
        build: Option<u32>,
        is_xp_i386: bool,
    ) -> Self {
        let xp = is_xp_i386 || major_version == Some(5);
        if xp {
            return Self {
                xp: true,
                ..Self::unknown()
            };
        }

        let windows_7 = matches!((major_version, minor_version), (Some(6), Some(1)));
        let vista_or_later = major_version.is_some_and(|major| major >= 6);
        let windows_7_or_later = major_version.is_some_and(|major| major > 6)
            || matches!((major_version, minor_version), (Some(6), Some(minor)) if minor >= 1);
        let windows_8_or_later = major_version.is_some_and(|major| major > 6)
            || matches!((major_version, minor_version), (Some(6), Some(minor)) if minor >= 2);
        let windows_81_or_later = major_version.is_some_and(|major| major > 6)
            || matches!((major_version, minor_version), (Some(6), Some(minor)) if minor >= 3);
        let windows_10_family = major_version.is_some_and(|major| major >= 10);
        let windows_11 = windows_10_family && build.is_some_and(|build| build >= 22_000);
        // lr-core selects the legacy WUA or modern USO removal profile from the target build and
        // rechecks that profile's service/file anchors before its first mutation.
        let windows_update_removal = build
            .map(lr_core::offline_windows_update_removal::supports_build_number)
            .unwrap_or(false);
        let reserved_storage = match (major_version, minor_version, build) {
            (Some(major), Some(minor), Some(build)) => {
                lr_core::reserved_storage::is_supported_target_version(
                    major.into(),
                    minor.into(),
                    build,
                )
            }
            _ => false,
        };

        Self {
            // Order matches AdvancedPageHandles::system_checks.
            system_options: [
                vista_or_later,                           // remove shortcut arrow
                windows_11,                               // restore classic context menu
                windows_11,                               // bypass NRO
                windows_update_removal,                   // remove Windows Update component
                windows_10_family || windows_8_or_later,  // remove Defender engine
                reserved_storage,                         // disable reserved storage
                vista_or_later,                           // disable UAC
                windows_10_family || windows_81_or_later, // disable device encryption
                windows_10_family || windows_8_or_later,  // remove provisioned UWP apps
                windows_7_or_later,                       // migrate Wi-Fi profile
            ],
            storage_controller_drivers: windows_10_family,
            windows_7,
            xp: false,
        }
    }

    pub fn supports_system_option(self, index: usize) -> bool {
        self.system_options.get(index).copied().unwrap_or(false)
    }
}

impl Default for AdvancedOptionsData {
    fn default() -> Self {
        Self {
            preserve_personal_files: false,
            remove_shortcut_arrow: true,
            restore_classic_context_menu: false,
            bypass_nro: true,
            disable_windows_update: false,
            disable_windows_defender: false,
            disable_reserved_storage: true,
            disable_uac: false,
            disable_device_encryption: true,
            remove_uwp_apps: false,
            preinstalled_software: Vec::new(),
            install_vmware_tools: true,
            // This is an intent default only. `apply_runtime_defaults` immediately captures the
            // current connected profile into session-only fields, or clears the intent when no
            // connected Wi-Fi profile can be read.
            migrate_wifi: true,
            wifi_profile_xml: String::new(),
            wifi_ssid: String::new(),
            wifi_detected: None,
            run_script_during_deploy: false,
            deploy_script_path: String::new(),
            run_script_first_login: false,
            first_login_script_path: String::new(),
            import_custom_drivers: false,
            custom_drivers_path: String::new(),
            import_storage_controller_drivers: false,
            import_registry_file: false,
            registry_file_path: String::new(),
            import_custom_files: false,
            custom_files_path: String::new(),
            custom_username: true,
            username: String::new(),
            builtin_administrator: BuiltInAdministratorOptions::default(),
            custom_volume_label: false,
            volume_label: String::new(),
            win7_inject_usb3_driver: false,
            win7_usb3_driver_path: String::new(),
            win7_inject_nvme_driver: false,
            win7_nvme_driver_path: String::new(),
            win7_fix_acpi_bsod: false,
            win7_fix_storage_bsod: false,
            win7_uefi_patch: false,
            xp_inject_usb3_driver: false,
            xp_inject_nvme_driver: false,
            xp_defaults_applied: false,
        }
    }
}

impl AdvancedOptionsData {
    /// Restores non-persistent defaults after config deserialization.
    ///
    /// Old configs can contain neither account mode or an empty volume label. The native page is
    /// a two-choice radio group, so the ordinary current-user path is the deterministic default.
    pub fn apply_runtime_defaults(&mut self) {
        // The checkbox preference may survive a restart, but the captured WLAN profile is
        // deliberately session-only (`serde(skip)`). Re-capture it for this process so the
        // default-on preference is executable rather than a stale checked box.
        if self.migrate_wifi && self.wifi_profile_xml.trim().is_empty() {
            match capture_runtime_wifi_profile() {
                Some((ssid, xml)) => {
                    self.wifi_detected = Some(true);
                    self.wifi_ssid = ssid;
                    self.wifi_profile_xml = xml;
                }
                None => {
                    self.migrate_wifi = false;
                    self.wifi_detected = Some(false);
                    self.wifi_ssid.clear();
                }
            }
        }
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

    /// Removes options that are not supported by the selected target from an install-intent copy.
    /// Persisted preferences are left untouched by callers so selecting another image can restore
    /// the user's prior choices.
    pub fn retain_supported_for_target(&mut self, capabilities: AdvancedOptionCapabilities) {
        // Keep the historical processor-power workaround available only for Windows 7. It is an
        // opt-in compatibility attempt, not a general ACPI firmware repair. The broad 0x7B
        // registry mutation and UefiSeven remain retired because storage drivers and boot mode
        // must be selected from verified hardware and image metadata.
        self.win7_fix_storage_bsod = false;
        self.win7_uefi_patch = false;
        let supported = capabilities.system_options;
        if !supported[0] {
            self.remove_shortcut_arrow = false;
        }
        if !supported[1] {
            self.restore_classic_context_menu = false;
        }
        if !supported[2] {
            self.bypass_nro = false;
        }
        if !supported[3] {
            self.disable_windows_update = false;
        }
        if !supported[4] {
            self.disable_windows_defender = false;
        }
        if !supported[5] {
            self.disable_reserved_storage = false;
        }
        if !supported[6] {
            self.disable_uac = false;
        }
        if !supported[7] {
            self.disable_device_encryption = false;
        }
        if !supported[8] {
            self.remove_uwp_apps = false;
        }
        if !supported[9] {
            self.migrate_wifi = false;
            self.wifi_profile_xml.clear();
            self.wifi_ssid.clear();
        }
        if !capabilities.storage_controller_drivers {
            self.import_storage_controller_drivers = false;
        }
        if !capabilities.windows_7 {
            self.win7_inject_usb3_driver = false;
            self.win7_inject_nvme_driver = false;
            self.win7_fix_acpi_bsod = false;
            self.win7_fix_storage_bsod = false;
            self.win7_uefi_patch = false;
        }
        if !capabilities.xp {
            self.xp_inject_usb3_driver = false;
            self.xp_inject_nvme_driver = false;
        }
    }

    pub fn update_supported_system_options(
        &mut self,
        capabilities: AdvancedOptionCapabilities,
        checked: [bool; ADVANCED_SYSTEM_OPTION_COUNT],
    ) {
        if capabilities.supports_system_option(0) {
            self.remove_shortcut_arrow = checked[0];
        }
        if capabilities.supports_system_option(1) {
            self.restore_classic_context_menu = checked[1];
        }
        if capabilities.supports_system_option(2) {
            self.bypass_nro = checked[2];
        }
        if capabilities.supports_system_option(3) {
            self.disable_windows_update = checked[3];
        }
        if capabilities.supports_system_option(4) {
            self.disable_windows_defender = checked[4];
        }
        if capabilities.supports_system_option(5) {
            self.disable_reserved_storage = checked[5];
        }
        if capabilities.supports_system_option(6) {
            self.disable_uac = checked[6];
        }
        if capabilities.supports_system_option(7) {
            self.disable_device_encryption = checked[7];
        }
        if capabilities.supports_system_option(8) {
            self.remove_uwp_apps = checked[8];
        }
        if capabilities.supports_system_option(9) {
            self.migrate_wifi = checked[9];
        }
    }
}

#[cfg(windows)]
fn capture_runtime_wifi_profile() -> Option<(String, String)> {
    super::native_wifi::capture_connected_wifi()
        .ok()
        .map(|profile| (profile.ssid, profile.xml))
}

#[cfg(not(windows))]
fn capture_runtime_wifi_profile() -> Option<(String, String)> {
    None
}

/// Returns a safe default name for the ordinary local account created by Windows Setup.
///
/// The current token username wins. If USER32/Advapi cannot provide it, use a concise system
/// manufacturer token such as ASUS or VMware, and finally the stable `User` fallback.
pub(crate) fn default_install_username() -> String {
    windows_login_username()
        .filter(|username| {
            lr_core::unattend_account::validate_unattended_local_account_name(username).is_ok()
        })
        .or_else(system_manufacturer_username)
        .filter(|username| {
            lr_core::unattend_account::validate_unattended_local_account_name(username).is_ok()
        })
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
            preserve_personal_files: value.preserve_personal_files,
            remove_shortcut_arrow: value.remove_shortcut_arrow,
            restore_classic_context_menu: value.restore_classic_context_menu,
            bypass_nro: value.bypass_nro,
            disable_windows_update: value.disable_windows_update,
            disable_windows_defender: value.disable_windows_defender,
            disable_reserved_storage: value.disable_reserved_storage,
            disable_uac: value.disable_uac,
            disable_device_encryption: value.disable_device_encryption,
            remove_uwp_apps: value.remove_uwp_apps,
            preinstalled_software: value.preinstalled_software.clone(),
            install_vmware_tools: value.install_vmware_tools,
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
    /// Runtime-only destructive layout. Disk selections and random locators are never persisted.
    #[serde(skip, default)]
    pub custom_install_plan: lr_core::custom_install::CustomInstallPlan,
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
            custom_install_plan: lr_core::custom_install::CustomInstallPlan::ReinstallPartition,
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
        assert!(prefs.advanced_options.remove_shortcut_arrow);
        assert!(prefs.advanced_options.bypass_nro);
        assert!(prefs.advanced_options.disable_reserved_storage);
        assert!(prefs.advanced_options.disable_device_encryption);
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

    #[test]
    fn advanced_defaults_enable_the_four_safe_install_preferences() {
        let data = AdvancedOptionsData::default();
        assert!(data.remove_shortcut_arrow);
        assert!(data.bypass_nro);
        assert!(data.disable_reserved_storage);
        assert!(data.disable_device_encryption);
        assert!(!data.disable_windows_update);
        assert!(!data.disable_windows_defender);
    }

    #[test]
    fn windows_7_capabilities_exclude_newer_windows_options() {
        let capabilities =
            AdvancedOptionCapabilities::for_target(Some(6), Some(1), Some(7_601), false);
        assert_eq!(
            capabilities.system_options,
            [true, false, false, true, false, false, true, false, false, true]
        );
        assert!(capabilities.windows_7);
        assert!(!capabilities.storage_controller_drivers);
        assert!(!capabilities.xp);
    }

    #[test]
    fn windows_11_capabilities_include_current_system_options() {
        let capabilities =
            AdvancedOptionCapabilities::for_target(Some(10), Some(0), Some(26_100), false);
        assert!(capabilities.system_options.into_iter().all(|value| value));
        assert!(capabilities.storage_controller_drivers);
        assert!(!capabilities.windows_7);
        assert!(!capabilities.xp);
    }

    #[test]
    fn intermediate_windows_versions_expose_only_options_the_target_supports() {
        let windows_8 =
            AdvancedOptionCapabilities::for_target(Some(6), Some(2), Some(9_200), false);
        assert_eq!(
            windows_8.system_options,
            [true, false, false, true, true, false, true, false, true, true]
        );

        let windows_81 =
            AdvancedOptionCapabilities::for_target(Some(6), Some(3), Some(9_600), false);
        assert_eq!(
            windows_81.system_options,
            [true, false, false, true, true, false, true, true, true, true]
        );

        let windows_10_1809 =
            AdvancedOptionCapabilities::for_target(Some(10), Some(0), Some(17_763), false);
        assert_eq!(
            windows_10_1809.system_options,
            [true, false, false, true, true, false, true, true, true, true]
        );

        let windows_10_1903 =
            AdvancedOptionCapabilities::for_target(Some(10), Some(0), Some(18_362), false);
        assert!(!windows_10_1903.system_options[5]);
        assert!(windows_10_1903.system_options[3]);
        assert!(!windows_10_1903.system_options[1]);
        assert!(!windows_10_1903.system_options[2]);

        let windows_10_2004 =
            AdvancedOptionCapabilities::for_target(Some(10), Some(0), Some(19_041), false);
        assert!(windows_10_2004.system_options[5]);
        assert!(windows_10_2004.system_options[3]);

        let windows_11_23h2 =
            AdvancedOptionCapabilities::for_target(Some(10), Some(0), Some(22_631), false);
        assert!(windows_11_23h2.system_options[3]);
    }

    #[test]
    fn intent_filter_clears_hidden_windows_7_incompatible_defaults() {
        let capabilities =
            AdvancedOptionCapabilities::for_target(Some(6), Some(1), Some(7_601), false);
        let mut data = AdvancedOptionsData {
            restore_classic_context_menu: true,
            disable_windows_defender: true,
            remove_uwp_apps: true,
            import_storage_controller_drivers: true,
            xp_inject_usb3_driver: true,
            ..AdvancedOptionsData::default()
        };
        data.retain_supported_for_target(capabilities);

        assert!(data.remove_shortcut_arrow);
        assert!(!data.restore_classic_context_menu);
        assert!(!data.bypass_nro);
        assert!(!data.disable_windows_defender);
        assert!(!data.disable_reserved_storage);
        assert!(!data.disable_device_encryption);
        assert!(!data.remove_uwp_apps);
        assert!(!data.import_storage_controller_drivers);
        assert!(!data.xp_inject_usb3_driver);
    }

    #[test]
    fn hidden_controls_do_not_destroy_preferences_for_another_image() {
        let capabilities =
            AdvancedOptionCapabilities::for_target(Some(6), Some(1), Some(7_601), false);
        let mut data = AdvancedOptionsData::default();
        data.update_supported_system_options(capabilities, [false; 10]);

        assert!(!data.remove_shortcut_arrow);
        assert!(!data.disable_windows_update);
        assert!(!data.disable_uac);
        assert!(!data.migrate_wifi);
        assert!(data.bypass_nro);
        assert!(data.disable_reserved_storage);
        assert!(data.disable_device_encryption);
    }

    #[test]
    fn runtime_defaults_drop_stale_wifi_migration_without_a_session_profile() {
        let mut data = AdvancedOptionsData {
            migrate_wifi: true,
            wifi_ssid: "stale-network".to_string(),
            ..AdvancedOptionsData::default()
        };

        data.apply_runtime_defaults();

        assert!(!data.migrate_wifi);
        assert!(data.wifi_ssid.is_empty());
    }

    #[test]
    fn runtime_defaults_keep_wifi_migration_with_a_captured_session_profile() {
        let mut data = AdvancedOptionsData {
            migrate_wifi: true,
            wifi_profile_xml: "<WLANProfile />".to_string(),
            wifi_ssid: "current-network".to_string(),
            ..AdvancedOptionsData::default()
        };

        data.apply_runtime_defaults();

        assert!(data.migrate_wifi);
        assert_eq!(data.wifi_ssid, "current-network");
    }
}
