//! Strict, versioned configuration used by the normal-Windows command line.
//!
//! This schema is deliberately independent from the long-lived GUI preferences and from the
//! PE handoff INI.  Parsing it never performs installation, backup, disk or registry I/O.

use anyhow::{anyhow, Context, Result};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub const CLI_CONFIG_SCHEMA_VERSION: u32 = 1;
const MAX_CLI_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliConfig {
    pub schema_version: u32,
    pub operation: CliOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CliOperation {
    Install(Box<InstallSpec>),
    Backup(BackupSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallSpec {
    pub target_partition: String,
    /// Destructive installation scope. Full-disk mode is never inherited from GUI preferences;
    /// it must be armed explicitly in this versioned document for the current run.
    #[serde(default)]
    pub install_mode: CliInstallMode,
    /// Current-session disk numbers explicitly confirmed for erasure. The disk containing
    /// `target_partition` becomes the sole Windows disk; any additional confirmed disks are data
    /// disks. These numbers are only used to build fresh random locator bindings before reboot.
    #[serde(default)]
    pub confirmed_disk_numbers: Vec<u32>,
    /// Integer GiB requested for the new Windows volume in `dual_boot` mode. The planner still
    /// enforces the currently selected image's exact expanded-size budget before any shrink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dual_boot_size_gib: Option<u64>,
    pub image_path: String,
    #[serde(default)]
    pub image_backing_path: String,
    #[serde(default = "default_volume_index")]
    pub volume_index: u32,
    #[serde(default = "default_true")]
    pub format_partition: bool,
    #[serde(default = "default_true")]
    pub repair_boot: bool,
    #[serde(default)]
    pub unattended: bool,
    #[serde(default)]
    pub auto_reboot: bool,
    /// Disposable-VM automation only: a terminal normal/PE failure powers the machine off, while
    /// a successful PE install continues into the new system and powers off after the authenticated
    /// first-logon finalizer has attempted every selected software package.
    #[serde(default)]
    pub automation_shutdown_on_terminal: bool,
    #[serde(default)]
    pub driver_action: CliDriverAction,
    #[serde(default)]
    pub boot_mode: CliBootMode,
    #[serde(default)]
    pub boot_pca_mode: CliBootPcaMode,
    #[serde(default)]
    pub custom_unattend_path: String,
    /// Reproduce the adjacent GUI `config.json` installation preferences.  Target/image fields
    /// remain authoritative in this versioned CLI document; all preference fields below them are
    /// taken from the required adjacent application configuration when this is true.
    #[serde(default)]
    pub inherit_app_install_prefs: bool,
    /// Stable v4 catalogue IDs selected for this run.  Catalogue URLs and silent commands are
    /// resolved freshly during planning and are never copied from a stale local preference file.
    #[serde(default)]
    pub preinstalled_software_ids: Vec<String>,
    #[serde(default)]
    pub advanced: AdvancedSpec,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliInstallMode {
    #[default]
    ReinstallPartition,
    RepartitionAllDisks,
    DualBoot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AdvancedSpec {
    /// Current-run-only reinstall mode; never inherited implicitly from GUI preferences.
    #[serde(default)]
    pub preserve_personal_files: bool,
    #[serde(default)]
    pub remove_shortcut_arrow: bool,
    #[serde(default)]
    pub restore_classic_context_menu: bool,
    #[serde(default)]
    pub bypass_nro: bool,
    #[serde(default)]
    pub disable_windows_update: bool,
    #[serde(default)]
    pub disable_windows_defender: bool,
    #[serde(default)]
    pub disable_reserved_storage: bool,
    #[serde(default)]
    pub disable_uac: bool,
    #[serde(default)]
    pub disable_device_encryption: bool,
    #[serde(default)]
    pub remove_uwp_apps: bool,
    /// Exact current-session Wi-Fi profile captured by the GUI exporter. The XML may contain a
    /// clear-text network key, so the config is protected by `write_atomic` and redacted in output.
    #[serde(default)]
    pub migrate_wifi: bool,
    #[serde(default)]
    pub wifi_ssid: String,
    #[serde(default)]
    pub wifi_profile_xml: String,
    /// Separate server-authorized VMware Tools option; valid only on positively detected VMware.
    #[serde(default)]
    pub install_vmware_tools: bool,
    #[serde(default)]
    pub deploy_script_path: String,
    #[serde(default)]
    pub first_login_script_path: String,
    #[serde(default)]
    pub custom_drivers_path: String,
    #[serde(default)]
    pub import_storage_controller_drivers: bool,
    #[serde(default)]
    pub registry_file_path: String,
    #[serde(default)]
    pub custom_files_path: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub builtin_administrator: BuiltInAdministratorSpec,
    #[serde(default)]
    pub volume_label: String,
    #[serde(default)]
    pub win7_fix_acpi_bsod: bool,
    #[serde(default)]
    pub win7_inject_usb3_driver: bool,
    #[serde(default)]
    pub win7_usb3_driver_path: String,
    #[serde(default)]
    pub win7_inject_nvme_driver: bool,
    #[serde(default)]
    pub win7_nvme_driver_path: String,
    #[serde(default)]
    pub win7_fix_storage_bsod: bool,
    #[serde(default)]
    pub win7_uefi_patch: bool,
    #[serde(default)]
    pub xp_inject_usb3_driver: bool,
    #[serde(default)]
    pub xp_inject_nvme_driver: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltInAdministratorSpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_administrator_name")]
    pub account_name: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_true")]
    pub auto_logon: bool,
}

impl Default for BuiltInAdministratorSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            account_name: default_administrator_name(),
            password: String::new(),
            auto_logon: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliDriverAction {
    None,
    SaveOnly,
    #[default]
    AutoImport,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBootMode {
    #[default]
    Auto,
    Uefi,
    Legacy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBootPcaMode {
    #[default]
    Auto,
    Pca2011,
    Pca2023,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSpec {
    pub source_partition: String,
    pub save_path: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub format: CliBackupFormat,
    #[serde(default)]
    pub execution_mode: CliBackupExecutionMode,
    #[serde(default)]
    pub output_policy: CliBackupOutputPolicy,
    #[serde(default)]
    pub auto_reboot: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBackupFormat {
    #[default]
    Wim,
    Esd,
}

impl CliBackupFormat {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Wim => 0,
            Self::Esd => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBackupExecutionMode {
    #[default]
    Auto,
    Direct,
    ViaPe,
}

impl CliBackupExecutionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Direct => "direct",
            Self::ViaPe => "via_pe",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBackupOutputPolicy {
    #[default]
    Create,
    Replace,
    Append,
}

impl CliBackupOutputPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Replace => "replace",
            Self::Append => "append",
        }
    }
}

const fn default_true() -> bool {
    true
}
const fn default_volume_index() -> u32 {
    1
}
fn default_administrator_name() -> String {
    "Administrator".to_owned()
}

impl CliConfig {
    pub fn parse(text: &str) -> Result<Self> {
        // `serde_json` normally accepts duplicate object keys with last-value-wins semantics.
        // A destructive configuration must never have two plausible readings, so reject
        // duplicates recursively before converting the value into the versioned DTO.
        let mut deserializer = serde_json::Deserializer::from_str(text);
        let strict = StrictJsonValue::deserialize(&mut deserializer)
            .context("invalid CLI configuration JSON")?;
        deserializer
            .end()
            .context("invalid trailing CLI configuration content")?;
        let mut config: Self =
            serde_json::from_value(strict.0).context("invalid CLI configuration schema")?;
        if config.schema_version != CLI_CONFIG_SCHEMA_VERSION {
            return Err(anyhow!(
                "unsupported schema_version {}; expected {}",
                config.schema_version,
                CLI_CONFIG_SCHEMA_VERSION
            ));
        }
        config.normalize()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        validate_local_absolute_path_str(
            path.to_str()
                .ok_or_else(|| anyhow!("CLI configuration path is not Unicode"))?,
            "CLI configuration path",
        )?;
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let pinned = lr_core::scoped_temp_file::pin_existing_directory_ancestors(parent)
            .with_context(|| {
                format!("failed to pin CLI configuration path {}", parent.display())
            })?;
        pinned
            .verify_unchanged()
            .context("CLI configuration path changed before open")?;
        let mut file = open_plain_regular_file(path)?;
        pinned
            .verify_unchanged()
            .context("CLI configuration path changed during open")?;
        let size = file.metadata()?.len();
        if size > MAX_CLI_CONFIG_BYTES {
            return Err(anyhow!(
                "CLI configuration exceeds the {} byte limit",
                MAX_CLI_CONFIG_BYTES
            ));
        }
        let mut input = String::with_capacity(size as usize);
        file.read_to_string(&mut input)
            .with_context(|| format!("failed to read CLI configuration {}", path.display()))?;
        pinned
            .verify_unchanged()
            .context("CLI configuration path changed during read")?;
        Self::parse(&input)
    }

    pub fn normalize(&mut self) -> Result<()> {
        match &mut self.operation {
            CliOperation::Install(spec) => {
                spec.target_partition = normalize_drive(&spec.target_partition)?;
                let mut confirmed_disks = std::collections::BTreeSet::new();
                for disk in &spec.confirmed_disk_numbers {
                    if !confirmed_disks.insert(*disk) {
                        return Err(anyhow!(
                            "confirmed_disk_numbers contains duplicate disk {disk}"
                        ));
                    }
                }
                if spec.confirmed_disk_numbers.len() > 32 {
                    return Err(anyhow!("too many confirmed_disk_numbers entries"));
                }
                match spec.install_mode {
                    CliInstallMode::ReinstallPartition => {
                        if !spec.confirmed_disk_numbers.is_empty() {
                            return Err(anyhow!(
                                "confirmed_disk_numbers requires install_mode=repartition_all_disks"
                            ));
                        }
                        if spec.dual_boot_size_gib.is_some() {
                            return Err(anyhow!(
                                "dual_boot_size_gib requires install_mode=dual_boot"
                            ));
                        }
                    }
                    CliInstallMode::RepartitionAllDisks => {
                        if spec.confirmed_disk_numbers.is_empty() {
                            return Err(anyhow!(
                                "install_mode=repartition_all_disks requires confirmed_disk_numbers"
                            ));
                        }
                        if spec.dual_boot_size_gib.is_some() {
                            return Err(anyhow!(
                                "dual_boot_size_gib requires install_mode=dual_boot"
                            ));
                        }
                    }
                    CliInstallMode::DualBoot => {
                        if !spec.confirmed_disk_numbers.is_empty() {
                            return Err(anyhow!(
                                "confirmed_disk_numbers is not valid for install_mode=dual_boot"
                            ));
                        }
                        let size_gib = spec.dual_boot_size_gib.ok_or_else(|| {
                            anyhow!("install_mode=dual_boot requires dual_boot_size_gib")
                        })?;
                        if size_gib == 0
                            || size_gib.checked_mul(lr_core::custom_install::GIB).is_none()
                        {
                            return Err(anyhow!(
                                "dual_boot_size_gib must be a positive supported integer GiB value"
                            ));
                        }
                        if !spec.repair_boot {
                            return Err(anyhow!(
                                "install_mode=dual_boot requires repair_boot=true"
                            ));
                        }
                    }
                }
                spec.image_path = normalize_required_path(&spec.image_path, "image_path")?;
                spec.image_backing_path = normalize_optional_path(&spec.image_backing_path);
                spec.custom_unattend_path = normalize_optional_path(&spec.custom_unattend_path);
                let mut software_ids = std::collections::BTreeSet::new();
                for id in &mut spec.preinstalled_software_ids {
                    *id = id.trim().to_ascii_lowercase();
                    if id.is_empty()
                        || id.len() > 128
                        || !id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                        })
                    {
                        return Err(anyhow!("invalid preinstalled_software_ids entry"));
                    }
                    if !software_ids.insert(id.clone()) {
                        return Err(anyhow!(
                            "preinstalled_software_ids contains duplicate id {id}"
                        ));
                    }
                }
                if spec.preinstalled_software_ids.len()
                    > lr_core::software_install::MAX_SELECTED_SOFTWARE_PACKAGES
                {
                    return Err(anyhow!("too many preinstalled_software_ids entries"));
                }
                if !spec.custom_unattend_path.is_empty() && !spec.unattended {
                    return Err(anyhow!("custom_unattend_path requires unattended=true"));
                }
                if spec.volume_index == 0 {
                    return Err(anyhow!("volume_index must be greater than zero"));
                }
                validate_optional_local_path(&spec.image_backing_path, "image_backing_path")?;
                validate_optional_local_path(&spec.custom_unattend_path, "custom_unattend_path")?;
                normalize_advanced(&mut spec.advanced)?;
            }
            CliOperation::Backup(spec) => {
                spec.source_partition = normalize_drive(&spec.source_partition)?;
                spec.save_path = normalize_required_path(&spec.save_path, "save_path")?;
                spec.name = spec.name.trim().to_owned();
                spec.description = spec.description.trim().to_owned();
                if spec.name.is_empty() {
                    return Err(anyhow!("name must not be empty"));
                }
                if spec.execution_mode == CliBackupExecutionMode::Direct && spec.auto_reboot {
                    return Err(anyhow!(
                        "auto_reboot is only meaningful when execution_mode may use via_pe"
                    ));
                }
                let expected_extension = match spec.format {
                    CliBackupFormat::Wim => "wim",
                    CliBackupFormat::Esd => "esd",
                };
                let actual_extension = Path::new(&spec.save_path)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if !actual_extension.eq_ignore_ascii_case(expected_extension) {
                    return Err(anyhow!(
                        "save_path extension must match backup format {expected_extension}"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn redacted_value(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("serializing a validated CLI config");
        if let Some(password) = value.pointer_mut("/operation/password") {
            *password = serde_json::Value::String("***REDACTED***".to_owned());
        }
        // `operation` is internally tagged, so install fields are siblings of `type`.
        if let Some(password) =
            value.pointer_mut("/operation/advanced/builtin_administrator/password")
        {
            if password.as_str().is_some_and(|secret| !secret.is_empty()) {
                *password = serde_json::Value::String("***REDACTED***".to_owned());
            }
        }
        if let Some(profile) = value.pointer_mut("/operation/advanced/wifi_profile_xml") {
            if profile.as_str().is_some_and(|secret| !secret.is_empty()) {
                *profile = serde_json::Value::String("***REDACTED***".to_owned());
            }
        }
        value
    }

    pub fn write_atomic(&self, path: &Path, force: bool) -> Result<()> {
        validate_local_absolute_path_str(
            path.to_str()
                .ok_or_else(|| anyhow!("CLI output path is not Unicode"))?,
            "CLI output path",
        )?;
        if path.exists() && !force {
            return Err(anyhow!(
                "{} already exists; pass --force to replace it",
                path.display()
            ));
        }
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        reject_existing_reparse_ancestors(parent)?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        reject_existing_reparse_ancestors(parent)?;
        if path.exists() {
            let _ = require_plain_regular_file(path)?;
        }
        let pinned_parent = lr_core::scoped_temp_file::pin_existing_directory_ancestors(parent)
            .with_context(|| format!("failed to pin output directory {}", parent.display()))?;
        pinned_parent
            .verify_unchanged()
            .context("output directory changed before temporary configuration creation")?;
        let bytes =
            serde_json::to_vec_pretty(self).context("failed to serialize CLI configuration")?;
        let result = (|| -> Result<()> {
            let (temporary, mut file) =
                lr_core::scoped_temp_file::ScopedTempFile::create_protected_writer_in(
                    parent,
                    "letrecovery-cli-config",
                    "json",
                )
                .with_context(|| {
                    format!(
                        "failed to create temporary configuration in {}",
                        parent.display()
                    )
                })?;
            file.write_all(&bytes)
                .context("failed to write temporary CLI configuration")?;
            file.sync_all()
                .context("failed to flush temporary CLI configuration")?;
            drop(file);
            let readback = CliConfig::load(temporary.path())?;
            if &readback != self {
                return Err(anyhow!("temporary CLI configuration readback mismatch"));
            }
            pinned_parent
                .verify_unchanged()
                .context("output directory changed before configuration publication")?;
            publish_temporary(temporary.path(), path, force).with_context(|| {
                format!("failed to publish CLI configuration {}", path.display())
            })?;
            pinned_parent
                .verify_unchanged()
                .context("output directory changed during configuration publication")?;
            verify_sensitive_config_acl(path).with_context(|| {
                format!(
                    "published CLI configuration does not have the required protected ACL: {}",
                    path.display()
                )
            })?;
            let published = CliConfig::load(path)?;
            if &published != self {
                return Err(anyhow!("published CLI configuration readback mismatch"));
            }
            Ok(())
        })();
        result
    }
}

#[cfg(windows)]
pub(crate) fn publish_temporary(source: &Path, target: &Path, force: bool) -> std::io::Result<()> {
    if force {
        return lr_core::scoped_temp_file::atomic_replace_path(source, target);
    }
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))
}

#[cfg(not(windows))]
pub(crate) fn publish_temporary(source: &Path, target: &Path, force: bool) -> std::io::Result<()> {
    if force {
        lr_core::scoped_temp_file::atomic_replace_path(source, target)
    } else {
        std::fs::hard_link(source, target)?;
        std::fs::remove_file(source)
    }
}

pub fn require_plain_regular_file(path: &Path) -> Result<std::fs::Metadata> {
    let file = open_plain_regular_file(path)?;
    file.metadata()
        .with_context(|| format!("inspect regular file {}", path.display()))
}

/// Sensitive configurations may be consumed only when their persistent DACL has the exact
/// protected current-user + SYSTEM + Administrators access semantics installed by `write_atomic`.
/// SDDL is deliberately not used as an authority here: Windows may serialize the same binary SID
/// as either a numeric value or a well-known alias such as `LA`, `SY`, or `BA`. The verifier reads
/// the binary ACL and compares SIDs directly so account naming cannot change the result.
#[cfg(windows)]
pub(crate) fn verify_sensitive_config_acl(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetLengthSid,
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation, IsValidSid,
        IsWellKnownSid, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    struct LocalGuard(*mut std::ffi::c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0));
                }
            }
        }
    }

    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("open current token")?;
    let _token = HandleGuard(token);
    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(anyhow!("TokenUser result is too short"));
    }
    let mut bytes = vec![0u8; needed as usize];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(bytes.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
    }
    .context("read current TokenUser")?;
    let record = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<TOKEN_USER>()) };
    let current_sid = record.User.Sid;
    if current_sid.is_invalid() || !unsafe { IsValidSid(current_sid).as_bool() } {
        return Err(anyhow!("TokenUser returned an invalid SID"));
    }

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let result = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(anyhow!("read CLI config DACL failed: {}", result.0));
    }
    if descriptor.0.is_null() {
        return Err(anyhow!("GetNamedSecurityInfoW returned a null descriptor"));
    }
    let _descriptor_guard = LocalGuard(descriptor.0);

    let mut control = 0u16;
    let mut revision = 0u32;
    unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }
        .context("read CLI config security descriptor control")?;
    if control & SE_DACL_PROTECTED.0 == 0 {
        return Err(anyhow!("sensitive CLI config DACL is not protected"));
    }

    let mut dacl_present = false.into();
    let mut dacl_defaulted = false.into();
    let mut dacl = null_mut();
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    }
    .context("read CLI config DACL pointer")?;
    if !dacl_present.as_bool() || dacl.is_null() {
        return Err(anyhow!("sensitive CLI config has no non-null DACL"));
    }

    let mut information = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .context("read CLI config ACL size information")?;

    let current_is_system = unsafe { IsWellKnownSid(current_sid, WinLocalSystemSid).as_bool() };
    let current_is_administrators =
        unsafe { IsWellKnownSid(current_sid, WinBuiltinAdministratorsSid).as_bool() };
    let expected_ace_count = if current_is_system || current_is_administrators {
        2
    } else {
        3
    };
    if information.AceCount != expected_ace_count {
        return Err(anyhow!(
            "sensitive CLI config DACL has {} ACEs instead of the required unique trustee count {}",
            information.AceCount,
            expected_ace_count
        ));
    }

    let mut saw_current = false;
    let mut saw_system = false;
    let mut saw_administrators = false;
    for index in 0..information.AceCount {
        let mut raw_ace = null_mut();
        unsafe { GetAce(dacl, index, &mut raw_ace) }
            .with_context(|| format!("read CLI config DACL ACE {index}"))?;
        if raw_ace.is_null() {
            return Err(anyhow!("CLI config DACL returned a null ACE"));
        }
        let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
        if header.AceType != 0
            || header.AceFlags != 0
            || usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err(anyhow!(
                "sensitive CLI config DACL contains a non-exact allow ACE"
            ));
        }
        let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Mask != FILE_ALL_ACCESS.0 {
            return Err(anyhow!(
                "sensitive CLI config DACL contains a non-full-control ACE"
            ));
        }
        let sid = PSID((&ace.SidStart as *const u32).cast_mut().cast());
        if !unsafe { IsValidSid(sid).as_bool() } {
            return Err(anyhow!("sensitive CLI config DACL contains an invalid SID"));
        }
        let sid_offset = (&ace.SidStart as *const u32 as usize)
            .checked_sub(raw_ace as usize)
            .ok_or_else(|| anyhow!("sensitive CLI config ACE SID precedes its header"))?;
        let sid_end = sid_offset
            .checked_add(unsafe { GetLengthSid(sid) } as usize)
            .ok_or_else(|| anyhow!("sensitive CLI config ACE SID length overflow"))?;
        if sid_end > usize::from(header.AceSize) {
            return Err(anyhow!("sensitive CLI config ACE contains a truncated SID"));
        }

        let is_current = unsafe { EqualSid(sid, current_sid).is_ok() };
        let is_system = unsafe { IsWellKnownSid(sid, WinLocalSystemSid).as_bool() };
        let is_administrators =
            unsafe { IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool() };
        if !is_current && !is_system && !is_administrators {
            return Err(anyhow!(
                "sensitive CLI config DACL grants an unexpected principal"
            ));
        }
        if (is_current && saw_current)
            || (is_system && saw_system)
            || (is_administrators && saw_administrators)
        {
            return Err(anyhow!(
                "sensitive CLI config DACL contains a duplicate trustee ACE"
            ));
        }
        saw_current |= is_current;
        saw_system |= is_system;
        saw_administrators |= is_administrators;
    }
    if !saw_current || !saw_system || !saw_administrators {
        return Err(anyhow!(
            "sensitive CLI config DACL does not exactly grant current user, SYSTEM and Administrators"
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn verify_sensitive_config_acl(_path: &Path) -> Result<()> {
    Ok(())
}

pub(crate) fn open_plain_regular_file(path: &Path) -> Result<File> {
    reject_existing_reparse_ancestors(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        let file = options
            .open(path)
            .with_context(|| format!("open plain file {}", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(anyhow!(
                "path is not a regular non-reparse file: {}",
                path.display()
            ));
        }
        Ok(file)
    }
    #[cfg(not(windows))]
    {
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "path is not a regular non-symlink file: {}",
                path.display()
            ));
        }
        Ok(file)
    }
}

fn reject_existing_reparse_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata_is_reparse(&metadata) => {
                return Err(anyhow!(
                    "path contains a reparse point: {}",
                    ancestor.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect path ancestor {}", ancestor.display()))
            }
        }
    }
    Ok(())
}

fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn normalize_drive(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(anyhow!("partition must be a drive letter such as C:"));
    }
    Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
}

fn normalize_required_path(value: &str, field: &str) -> Result<String> {
    let normalized = normalize_optional_path(value);
    if normalized.is_empty() {
        Err(anyhow!("{field} must not be empty"))
    } else {
        validate_local_absolute_path_str(&normalized, field)?;
        Ok(normalized)
    }
}

fn normalize_optional_path(value: &str) -> String {
    value.trim().replace('/', "\\")
}

fn normalize_advanced(spec: &mut AdvancedSpec) -> Result<()> {
    spec.wifi_ssid = spec.wifi_ssid.trim().to_owned();
    if spec.migrate_wifi {
        if spec.wifi_ssid.is_empty() || spec.wifi_profile_xml.trim().is_empty() {
            return Err(anyhow!(
                "migrate_wifi requires both wifi_ssid and wifi_profile_xml"
            ));
        }
    } else {
        // Runtime-only credentials have no meaning unless migration is explicitly armed. Dropping
        // stale values also avoids carrying a clear-text network key in an unrelated export.
        spec.wifi_ssid.clear();
        spec.wifi_profile_xml.clear();
    }
    spec.deploy_script_path = normalize_optional_path(&spec.deploy_script_path);
    spec.first_login_script_path = normalize_optional_path(&spec.first_login_script_path);
    spec.custom_drivers_path = normalize_optional_path(&spec.custom_drivers_path);
    spec.registry_file_path = normalize_optional_path(&spec.registry_file_path);
    spec.custom_files_path = normalize_optional_path(&spec.custom_files_path);
    spec.win7_usb3_driver_path = normalize_optional_path(&spec.win7_usb3_driver_path);
    spec.win7_nvme_driver_path = normalize_optional_path(&spec.win7_nvme_driver_path);
    spec.username = spec.username.trim().to_owned();
    spec.volume_label = spec.volume_label.trim().to_owned();
    spec.builtin_administrator.account_name =
        spec.builtin_administrator.account_name.trim().to_owned();
    for (value, field) in [
        (&spec.deploy_script_path, "deploy_script_path"),
        (&spec.first_login_script_path, "first_login_script_path"),
        (&spec.custom_drivers_path, "custom_drivers_path"),
        (&spec.registry_file_path, "registry_file_path"),
        (&spec.custom_files_path, "custom_files_path"),
        (&spec.win7_usb3_driver_path, "win7_usb3_driver_path"),
        (&spec.win7_nvme_driver_path, "win7_nvme_driver_path"),
    ] {
        validate_optional_local_path(value, field)?;
    }
    Ok(())
}

fn validate_optional_local_path(value: &str, field: &str) -> Result<()> {
    if value.is_empty() {
        Ok(())
    } else {
        validate_local_absolute_path_str(value, field)
    }
}

/// Strict path grammar for any file that may influence a destructive CLI run.
pub(crate) fn validate_local_absolute_path_str(value: &str, field: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || value.starts_with("\\\\")
        || value.starts_with("\\\\?\\")
        || value.starts_with("\\\\.\\")
    {
        return Err(anyhow!("{field} must be an absolute local drive path"));
    }
    if value[2..].contains(':') {
        return Err(anyhow!("{field} must not contain an alternate data stream"));
    }
    for component in value[3..].split('\\') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(anyhow!("{field} contains an empty or relative component"));
        }
        if component.ends_with(' ') || component.ends_with('.') {
            return Err(anyhow!(
                "{field} contains a component ending in a dot or space"
            ));
        }
        if component.chars().any(|character| {
            character <= '\u{1f}' || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        }) {
            return Err(anyhow!(
                "{field} contains a character forbidden by Win32 file naming"
            ));
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || stem
            .strip_prefix("COM")
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
            || stem
                .strip_prefix("LPT")
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
            || matches!(
                stem.as_str(),
                "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³"
            );
        if reserved {
            return Err(anyhow!("{field} contains a reserved Windows device name"));
        }
    }
    Ok(())
}

/// JSON value deserializer that rejects duplicate keys at every object depth.
struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonValueVisitor)
    }
}

struct StrictJsonValueVisitor;

impl<'de> Visitor<'de> for StrictJsonValueVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| E::custom("non-finite JSON number"))?;
        Ok(StrictJsonValue(Value::Number(number)))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let value = object.next_value::<StrictJsonValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn dacl_sddl(path: &Path) -> String {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
        use windows::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let result = unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut descriptor,
            )
        };
        assert_eq!(result, ERROR_SUCCESS);
        let mut sddl = PWSTR::null();
        unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut sddl,
                None,
            )
        }
        .unwrap();
        let value = unsafe { sddl.to_string() }.unwrap();
        unsafe {
            let _ = LocalFree(HLOCAL(sddl.0.cast()));
            let _ = LocalFree(HLOCAL(descriptor.0));
        }
        value
    }

    #[test]
    fn rejects_unknown_fields_and_versions() {
        let unknown = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.wim","name":"a","extra":1}}"#;
        assert!(CliConfig::parse(unknown).is_err());
        let version = r#"{"schema_version":2,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.wim","name":"a"}}"#;
        assert!(CliConfig::parse(version).is_err());
    }

    #[test]
    fn rejects_duplicate_keys_at_every_depth() {
        let top_level = r#"{"schema_version":1,"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.wim","name":"a"}}"#;
        assert!(CliConfig::parse(top_level).is_err());
        let nested = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","source_partition":"D:","save_path":"E:\\a.wim","name":"a"}}"#;
        assert!(CliConfig::parse(nested).is_err());
    }

    #[test]
    fn normalizes_drive_and_redacts_password() {
        let input = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"c:\\","image_path":" D:/install.wim ","advanced":{"builtin_administrator":{"enabled":true,"account_name":"Admin","password":"secret"}}}}"#;
        let config = CliConfig::parse(input).unwrap();
        let CliOperation::Install(spec) = &config.operation else {
            panic!()
        };
        assert_eq!(spec.target_partition, "C:");
        assert_eq!(spec.image_path, "D:\\install.wim");
        let shown = config.redacted_value().to_string();
        assert!(!shown.contains("secret"));
        assert!(shown.contains("REDACTED"));
    }

    #[test]
    fn normalizes_stable_software_ids_and_rejects_duplicates_or_shell_text() {
        let input = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","inherit_app_install_prefs":true,"preinstalled_software_ids":[" ToDesk ","7ZIP-X64","bandizip-x64"]}}"#;
        let config = CliConfig::parse(input).unwrap();
        let CliOperation::Install(spec) = config.operation else {
            panic!()
        };
        assert!(spec.inherit_app_install_prefs);
        assert_eq!(
            spec.preinstalled_software_ids,
            ["todesk", "7zip-x64", "bandizip-x64"]
        );

        let duplicate = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","preinstalled_software_ids":["ToDesk","todesk"]}}"#;
        assert!(CliConfig::parse(duplicate).is_err());
        let command = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","preinstalled_software_ids":["todesk & calc"]}}"#;
        assert!(CliConfig::parse(command).is_err());
    }

    #[test]
    fn full_disk_scope_requires_explicit_unique_current_session_disks() {
        let valid = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"repartition_all_disks","confirmed_disk_numbers":[0],"image_path":"D:\\install.wim"}}"#;
        let config = CliConfig::parse(valid).unwrap();
        let CliOperation::Install(spec) = config.operation else {
            panic!()
        };
        assert_eq!(spec.install_mode, CliInstallMode::RepartitionAllDisks);
        assert_eq!(spec.confirmed_disk_numbers, [0]);

        let missing = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"repartition_all_disks","image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(missing).is_err());
        let stale = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","confirmed_disk_numbers":[0],"image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(stale).is_err());
        let duplicate = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"repartition_all_disks","confirmed_disk_numbers":[0,0],"image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(duplicate).is_err());
    }

    #[test]
    fn dual_boot_scope_requires_only_a_positive_capacity_and_boot_repair() {
        let valid = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"dual_boot","dual_boot_size_gib":24,"image_path":"D:\\install.wim"}}"#;
        let config = CliConfig::parse(valid).unwrap();
        let CliOperation::Install(spec) = config.operation else {
            panic!()
        };
        assert_eq!(spec.install_mode, CliInstallMode::DualBoot);
        assert_eq!(spec.dual_boot_size_gib, Some(24));
        assert!(spec.confirmed_disk_numbers.is_empty());

        let missing = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"dual_boot","image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(missing).is_err());
        let zero = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"dual_boot","dual_boot_size_gib":0,"image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(zero).is_err());
        let stale_disk = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"dual_boot","dual_boot_size_gib":24,"confirmed_disk_numbers":[0],"image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(stale_disk).is_err());
        let no_boot_menu = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","install_mode":"dual_boot","dual_boot_size_gib":24,"repair_boot":false,"image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(no_boot_menu).is_err());
        let stale_size = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","dual_boot_size_gib":24,"image_path":"D:\\install.wim"}}"#;
        assert!(CliConfig::parse(stale_size).is_err());
    }

    #[test]
    fn removed_incremental_field_is_rejected_in_every_format() {
        let input = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.wim","name":"a","incremental":true}}"#;
        assert!(CliConfig::parse(input).is_err());
    }

    #[test]
    fn backup_defaults_are_auto_create_without_reboot() {
        let input = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.wim","name":"a"}}"#;
        let config = CliConfig::parse(input).unwrap();
        let CliOperation::Backup(spec) = config.operation else {
            panic!()
        };
        assert_eq!(spec.execution_mode, CliBackupExecutionMode::Auto);
        assert_eq!(spec.output_policy, CliBackupOutputPolicy::Create);
        assert!(!spec.auto_reboot);
    }

    #[test]
    fn backup_accepts_versioned_mode_and_policy_but_rejects_mismatched_semantics() {
        let input = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.esd","name":"a","format":"esd","execution_mode":"via_pe","output_policy":"append","auto_reboot":true}}"#;
        let config = CliConfig::parse(input).unwrap();
        let CliOperation::Backup(spec) = config.operation else {
            panic!()
        };
        assert_eq!(spec.execution_mode, CliBackupExecutionMode::ViaPe);
        assert_eq!(spec.output_policy, CliBackupOutputPolicy::Append);
        assert!(spec.auto_reboot);

        let direct_reboot = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.wim","name":"a","execution_mode":"direct","auto_reboot":true}}"#;
        assert!(CliConfig::parse(direct_reboot).is_err());
        let wrong_extension = r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"C:","save_path":"D:\\a.esd","name":"a","format":"wim"}}"#;
        assert!(CliConfig::parse(wrong_extension).is_err());
    }

    #[test]
    fn custom_unattend_requires_unattended_mode() {
        let input = r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","custom_unattend_path":"D:\\answer.xml"}}"#;
        assert!(CliConfig::parse(input).is_err());
    }

    #[test]
    fn destructive_cli_paths_reject_device_ads_relative_and_reserved_forms() {
        for path in [
            r"\\?\C:\image.wim",
            r"\\.\PhysicalDrive0",
            r"\\server\share\image.wim",
            r"C:\safe\..\image.wim",
            r"C:\safe\image.wim:stream",
            r"C:\safe\NUL.txt",
            r"C:\safe\COM1.log",
            r"C:\safe\trailing.\image.wim",
            r"C:\safe\bad?.wim",
            r"C:\safe\CONIN$",
            r"C:\safe\COM¹.txt",
        ] {
            assert!(
                validate_local_absolute_path_str(path, "test").is_err(),
                "accepted {path}"
            );
        }
        assert!(validate_local_absolute_path_str(r"D:\images\install.wim", "test").is_ok());
    }

    #[test]
    fn atomic_write_requires_force_and_reads_back() {
        let root = std::env::temp_dir().join(format!("lr-cli-config-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let path = root.join("config.json");
        #[cfg(windows)]
        let parent_dacl_before = dacl_sddl(&root);
        let config = CliConfig::parse(r#"{"schema_version":1,"operation":{"type":"backup","source_partition":"c:","save_path":"D:\\a.wim","name":"a"}}"#).unwrap();
        config.write_atomic(&path, true).unwrap();
        assert_eq!(CliConfig::load(&path).unwrap(), config);
        assert!(config.write_atomic(&path, false).is_err());
        #[cfg(windows)]
        assert_eq!(dacl_sddl(&root), parent_dacl_before);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&root);
    }

    #[cfg(windows)]
    #[test]
    fn generated_sensitive_config_has_a_verified_protected_acl() {
        let root =
            std::env::temp_dir().join(format!("lr-cli-sensitive-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let path = root.join("sensitive.json");
        let config = CliConfig::parse(r#"{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","advanced":{"builtin_administrator":{"enabled":true,"password":"secret"}}}}"#).unwrap();
        config.write_atomic(&path, true).unwrap();
        verify_sensitive_config_acl(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&root);
    }

    #[test]
    fn no_force_publish_never_replaces_an_existing_target() {
        let root = std::env::temp_dir().join(format!(
            "lr-cli-config-noreplace-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let source = root.join("source.tmp");
        let target = root.join("target.json");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        assert!(publish_temporary(&source, &target, false).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        let _ = std::fs::remove_file(source);
        let _ = std::fs::remove_file(target);
        let _ = std::fs::remove_dir(root);
    }
}
