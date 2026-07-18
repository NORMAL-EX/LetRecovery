//! Offline removal boundary for the Microsoft Defender Antivirus engine.
//!
//! This deliberately does not remove Windows Security, Firewall, SmartScreen, UAC, VBS,
//! System Guard, Web Threat Defense, Pluton, or Microsoft Defender for Endpoint components.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use anyhow::{Context, Result};

use crate::registry::OfflineRegistry;

const ENGINE_SERVICES: [&str; 8] = [
    "WinDefend",
    "WdBoot",
    "WdFilter",
    "WdNisDrv",
    "WdNisSvc",
    "WdAiNisDrv",
    "WdDevFlt",
    "KslD",
];

#[cfg(test)]
const PRESERVED_SECURITY_SERVICES: [&str; 8] = [
    "SecurityHealthService",
    "wscsvc",
    "mpssvc",
    "SgrmAgent",
    "SgrmBroker",
    "webthreatdefsvc",
    "MsSecFlt",
    "Sense",
];

const ENGINE_DIRECTORIES: [&str; 5] = [
    "ProgramData\\Microsoft\\Windows Defender",
    "Program Files\\Windows Defender",
    "Program Files (x86)\\Windows Defender",
    "Windows\\System32\\drivers\\wd",
    "Windows\\System32\\Tasks\\Microsoft\\Windows\\Windows Defender",
];

const ENGINE_DRIVER_FILES: [&str; 3] = [
    "Windows\\System32\\drivers\\WdBoot.sys",
    "Windows\\System32\\drivers\\WdFilter.sys",
    "Windows\\System32\\drivers\\WdNisDrv.sys",
];

const POLICY_DWORDS: [(&str, &str, u32); 16] = [
    (
        "Policies\\Microsoft\\Windows Defender",
        "DisableAntiSpyware",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender",
        "DisableAntiVirus",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender",
        "DisableRoutinelyTakingAction",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender",
        "ServiceKeepAlive",
        0,
    ),
    (
        "Policies\\Microsoft\\Windows Defender",
        "AllowFastServiceStartup",
        0,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
        "DisableRealtimeMonitoring",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
        "DisableBehaviorMonitoring",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
        "DisableOnAccessProtection",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
        "DisableScanOnRealtimeEnable",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
        "DisableIOAVProtection",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Spynet",
        "DisableBlockAtFirstSeen",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Spynet",
        "SpynetReporting",
        0,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Spynet",
        "SubmitSamplesConsent",
        2,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Signature Updates",
        "RealtimeSignatureDelivery",
        0,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Signature Updates",
        "UpdateOnStartUp",
        0,
    ),
    (
        "Policies\\Microsoft\\Windows Defender\\Signature Updates",
        "DisableScanOnUpdate",
        1,
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefenderRemovalPlan {
    target_root: PathBuf,
    service_keys: Vec<String>,
    removal_paths: Vec<PathBuf>,
    task_cache_key: String,
    engine_software_key: String,
}

impl DefenderRemovalPlan {
    fn new(
        target_root: PathBuf,
        software_hive_alias: &str,
        system_hive_alias: &str,
        control_sets: impl IntoIterator<Item = u32>,
    ) -> Result<Self> {
        validate_hive_alias(software_hive_alias)?;
        validate_hive_alias(system_hive_alias)?;

        let control_sets = control_sets
            .into_iter()
            .filter(|value| (1..=999).contains(value))
            .collect::<BTreeSet<_>>();
        if control_sets.is_empty() {
            anyhow::bail!("offline SYSTEM hive did not expose an active control set");
        }

        let service_keys = control_sets
            .iter()
            .flat_map(|control_set| {
                ENGINE_SERVICES.iter().map(move |service| {
                    format!(
                        "HKLM\\{}\\ControlSet{:03}\\Services\\{}",
                        system_hive_alias, control_set, service
                    )
                })
            })
            .collect();
        let removal_paths = ENGINE_DIRECTORIES
            .iter()
            .chain(ENGINE_DRIVER_FILES.iter())
            .map(|relative| target_root.join(relative))
            .collect();
        let task_cache_key = format!(
            "HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion\\Schedule\\TaskCache\\Tree\\Microsoft\\Windows\\Windows Defender",
            software_hive_alias
        );
        let engine_software_key =
            format!("HKLM\\{}\\Microsoft\\Windows Defender", software_hive_alias);

        Ok(Self {
            target_root,
            service_keys,
            removal_paths,
            task_cache_key,
            engine_software_key,
        })
    }

    pub fn service_keys(&self) -> &[String] {
        &self.service_keys
    }

    pub fn removal_paths(&self) -> &[PathBuf] {
        &self.removal_paths
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefenderRemovalReport {
    pub disabled_services: usize,
    pub deleted_service_keys: usize,
    pub removed_paths: usize,
    pub deleted_task_cache: bool,
    pub deleted_task_records: usize,
    pub deleted_engine_software_key: bool,
}

fn validate_hive_alias(alias: &str) -> Result<()> {
    if alias.is_empty()
        || !alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("invalid offline registry hive alias: {alias:?}");
    }
    Ok(())
}

fn normalized_target_root(target_partition: &str) -> Result<PathBuf> {
    let value = target_partition.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        anyhow::bail!("target partition must be a drive letter, got {target_partition:?}");
    }
    let root = PathBuf::from(format!("{}\\", value.to_ascii_uppercase()));
    let system_hive = root.join("Windows\\System32\\config\\SYSTEM");
    let software_hive = root.join("Windows\\System32\\config\\SOFTWARE");
    if !system_hive.is_file() || !software_hive.is_file() {
        anyhow::bail!(
            "target does not contain complete offline registry hives: {}",
            root.display()
        );
    }
    Ok(root)
}

fn discover_control_sets(system_hive_alias: &str) -> Result<Vec<u32>> {
    let select_key = format!("HKLM\\{}\\Select", system_hive_alias);
    let mut values = BTreeSet::new();
    for name in ["Current", "Default", "LastKnownGood"] {
        match OfflineRegistry::query_dword(&select_key, name) {
            Ok(value) if (1..=999).contains(&value) => {
                values.insert(value);
            }
            Ok(value) => log::warn!(
                "offline SYSTEM Select\\{} contains an invalid control-set index: {}",
                name,
                value
            ),
            Err(error) => log::warn!(
                "offline SYSTEM Select\\{} could not be read and was skipped: {}",
                name,
                error
            ),
        }
    }
    if values.is_empty() {
        anyhow::bail!("failed to identify any active control set in {select_key}");
    }
    Ok(values.into_iter().collect())
}

#[cfg(windows)]
fn enable_file_removal_privileges() -> Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, HANDLE,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
        TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct TokenGuard(HANDLE);
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .context("OpenProcessToken for Defender engine removal")?;
    }
    let _guard = TokenGuard(token);
    for privilege_name in [SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME] {
        let mut luid = Default::default();
        unsafe {
            LookupPrivilegeValueW(PCWSTR::null(), privilege_name, &mut luid)
                .context("LookupPrivilegeValueW for Defender engine removal")?;
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            SetLastError(ERROR_SUCCESS);
            AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
                .context("AdjustTokenPrivileges for Defender engine removal")?;
            if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                anyhow::bail!(
                    "current process does not hold a required Defender removal privilege"
                );
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn enable_file_removal_privileges() -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn grant_administrators_full_control(path: &Path, is_directory: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
    use windows::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
        GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_GROUP, TRUSTEE_IS_SID,
        TRUSTEE_W,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, WinBuiltinAdministratorsSid, CONTAINER_INHERIT_ACE,
        DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE,
    };
    use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    struct AclGuard(*mut windows::Win32::Security::ACL);
    impl Drop for AclGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0.cast()));
                }
            }
        }
    }

    struct SecurityDescriptorGuard(PSECURITY_DESCRIPTOR);
    impl Drop for SecurityDescriptorGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 .0));
                }
            }
        }
    }

    let mut sid_buffer = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut sid_size = sid_buffer.len() as u32;
    let administrators_sid = PSID(sid_buffer.as_mut_ptr().cast());
    unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            PSID::default(),
            administrators_sid,
            &mut sid_size,
        )
        .context("CreateWellKnownSid for BUILTIN\\Administrators")?;
    }

    let inheritance = if is_directory {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        Default::default()
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            administrators_sid,
            PSID::default(),
            None,
            None,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "SetNamedSecurityInfoW(owner) failed for {} with Win32 error {}",
            path.display(),
            result.0
        );
    }

    let mut old_acl = null_mut();
    let mut security_descriptor = PSECURITY_DESCRIPTOR::default();
    let result = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_acl),
            None,
            &mut security_descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "GetNamedSecurityInfoW(DACL) failed for {} with Win32 error {}",
            path.display(),
            result.0
        );
    }
    let _security_descriptor_guard = SecurityDescriptorGuard(security_descriptor);
    if old_acl.is_null() {
        return Ok(());
    }

    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS.0,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_GROUP,
            ptstrName: PWSTR(administrators_sid.0.cast()),
        },
    };
    let mut acl = null_mut();
    let result = unsafe { SetEntriesInAclW(Some(&[access]), Some(old_acl.cast_const()), &mut acl) };
    if result != ERROR_SUCCESS {
        anyhow::bail!("SetEntriesInAclW failed with Win32 error {}", result.0);
    }
    let _acl_guard = AclGuard(acl);
    let result = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            PSID::default(),
            PSID::default(),
            Some(acl.cast_const()),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "SetNamedSecurityInfoW(DACL) failed for {} with Win32 error {}",
            path.display(),
            result.0
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn grant_administrators_full_control(_path: &Path, _is_directory: bool) -> Result<()> {
    Ok(())
}

fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn remove_tree_without_following_reparse_points(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("enumerate directory {}", path.display()))?
    {
        let entry = entry.with_context(|| format!("enumerate entry below {}", path.display()))?;
        let child = entry.path();
        let metadata = std::fs::symlink_metadata(&child)
            .with_context(|| format!("inspect {}", child.display()))?;
        if metadata_is_reparse_point(&metadata) {
            anyhow::bail!(
                "refusing to traverse a reparse point below Defender path: {}",
                child.display()
            );
        }
        if metadata.is_dir() {
            remove_tree_without_following_reparse_points(&child)?;
        } else if metadata.is_file() {
            std::fs::remove_file(&child)
                .with_context(|| format!("remove file {}", child.display()))?;
        } else {
            anyhow::bail!("refusing to remove a non-file path: {}", child.display());
        }
    }
    std::fs::remove_dir(path).with_context(|| format!("remove directory {}", path.display()))
}

fn prepare_tree_for_removal(path: &Path) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if metadata_is_reparse_point(&metadata) {
        anyhow::bail!("refusing to prepare a reparse point: {}", path.display());
    }
    grant_administrators_full_control(path, metadata.is_dir())?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("enumerate directory {}", path.display()))?
        {
            prepare_tree_for_removal(&entry?.path())?;
        }
    } else if !metadata.is_file() {
        anyhow::bail!("refusing to prepare a non-file path: {}", path.display());
    }
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("clear read-only attribute on {}", path.display()))?;
    }
    Ok(())
}

fn remove_owned_path(path: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata_is_reparse_point(&metadata) {
        anyhow::bail!(
            "refusing to remove a reparse/symlink target: {}",
            path.display()
        );
    }
    let is_directory = metadata.is_dir();
    prepare_tree_for_removal(path)?;
    if is_directory {
        remove_tree_without_following_reparse_points(path)?;
    } else if metadata.is_file() {
        std::fs::remove_file(path).with_context(|| format!("remove file {}", path.display()))?;
    } else {
        anyhow::bail!("refusing to remove a non-file path: {}", path.display());
    }
    if path.exists() {
        anyhow::bail!("path still exists after removal: {}", path.display());
    }
    Ok(true)
}

fn is_braced_guid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 38 || bytes[0] != b'{' || bytes[37] != b'}' {
        return false;
    }
    bytes[1..37].iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

fn remove_task_cache_records(
    plan: &DefenderRemovalPlan,
    software_hive_alias: &str,
) -> Result<usize> {
    let task_ids = OfflineRegistry::query_string_values_recursive(&plan.task_cache_key, "Id")?;
    let task_cache_base = format!(
        "HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion\\Schedule\\TaskCache",
        software_hive_alias
    );
    let mut removed = 0;
    for task_id in task_ids.into_iter().collect::<BTreeSet<_>>() {
        if !is_braced_guid(&task_id) {
            anyhow::bail!("invalid Defender scheduled-task cache Id: {task_id:?}");
        }
        for category in ["Tasks", "Plain", "Boot", "Logon", "Maintenance"] {
            let key = format!("{}\\{}\\{}", task_cache_base, category, task_id);
            if OfflineRegistry::delete_key_verified(&key)? {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn apply_policy_values(software_hive_alias: &str) -> Result<()> {
    let base = format!("HKLM\\{}", software_hive_alias);
    for (relative_key, value_name, value) in POLICY_DWORDS {
        OfflineRegistry::set_dword(&format!("{}\\{}", base, relative_key), value_name, value)
            .with_context(|| format!("set Defender policy {relative_key}\\{value_name}"))?;
    }
    Ok(())
}

/// Deeply remove only the offline Microsoft Defender Antivirus engine.
///
/// The caller must have already loaded the target SOFTWARE and SYSTEM hives under the supplied
/// aliases. The target is restricted to a drive-letter root containing complete registry hives.
pub fn remove_offline_defender_engine(
    target_partition: &str,
    software_hive_alias: &str,
    system_hive_alias: &str,
) -> Result<DefenderRemovalReport> {
    validate_hive_alias(software_hive_alias)?;
    validate_hive_alias(system_hive_alias)?;
    let target_root = normalized_target_root(target_partition)?;
    let control_sets = discover_control_sets(system_hive_alias)?;
    let plan = DefenderRemovalPlan::new(
        target_root.clone(),
        software_hive_alias,
        system_hive_alias,
        control_sets,
    )?;

    apply_policy_values(software_hive_alias)?;

    let mut disabled_services = 0;
    for key in &plan.service_keys {
        if OfflineRegistry::key_exists(key)? {
            OfflineRegistry::set_dword(key, "Start", 4)
                .with_context(|| format!("disable Defender engine service {key}"))?;
            disabled_services += 1;
        }
    }

    let mut removed_paths = 0;
    if plan.removal_paths.iter().any(|path| path.exists()) {
        enable_file_removal_privileges()?;
    }
    for path in &plan.removal_paths {
        if !path.starts_with(&plan.target_root) {
            anyhow::bail!(
                "Defender removal path escaped target root: {}",
                path.display()
            );
        }
        if remove_owned_path(path)? {
            removed_paths += 1;
        }
    }

    let deleted_task_records = remove_task_cache_records(&plan, software_hive_alias)?;
    let deleted_task_cache = OfflineRegistry::delete_key_verified(&plan.task_cache_key)?;
    let deleted_engine_software_key =
        OfflineRegistry::delete_key_verified(&plan.engine_software_key)?;
    let mut deleted_service_keys = 0;
    for key in &plan.service_keys {
        if OfflineRegistry::delete_key_verified(key)? {
            deleted_service_keys += 1;
        }
    }

    for key in &plan.service_keys {
        if OfflineRegistry::key_exists(key)? {
            anyhow::bail!("Defender engine service key survived removal: {key}");
        }
    }
    for path in &plan.removal_paths {
        if path.exists() {
            anyhow::bail!("Defender engine path survived removal: {}", path.display());
        }
    }

    Ok(DefenderRemovalReport {
        disabled_services,
        deleted_service_keys,
        removed_paths,
        deleted_task_cache,
        deleted_task_records,
        deleted_engine_software_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_plan_is_confined_and_excludes_other_security_components() {
        let root = PathBuf::from(r"C:\");
        let plan = DefenderRemovalPlan::new(root.clone(), "pc-soft", "pc-sys", [1, 2, 2]).unwrap();
        assert_eq!(plan.service_keys.len(), ENGINE_SERVICES.len() * 2);
        assert!(plan
            .service_keys
            .iter()
            .all(|key| ENGINE_SERVICES.iter().any(|name| key.ends_with(name))));
        assert!(PRESERVED_SECURITY_SERVICES.iter().all(|preserved| plan
            .service_keys
            .iter()
            .all(|key| !key.ends_with(preserved))));
        assert!(plan
            .removal_paths
            .iter()
            .all(|path| path.starts_with(&root)));
        assert!(plan
            .removal_paths
            .iter()
            .all(|path| !path.to_string_lossy().contains("SmartScreen")));
    }

    #[test]
    fn invalid_hive_aliases_and_control_sets_fail_closed() {
        assert!(
            DefenderRemovalPlan::new(PathBuf::from(r"C:\"), "pc-soft\\evil", "pc-sys", [1])
                .is_err()
        );
        assert!(
            DefenderRemovalPlan::new(PathBuf::from(r"C:\"), "pc-soft", "pc-sys", [0, 1000])
                .is_err()
        );
    }

    #[test]
    fn scheduled_task_ids_must_be_canonical_braced_guids() {
        assert!(is_braced_guid("{0ACC9108-2000-46C0-8407-5FD9F89521E8}"));
        assert!(!is_braced_guid("0ACC9108-2000-46C0-8407-5FD9F89521E8"));
        assert!(!is_braced_guid("{..\\Windows Defender}"));
    }
}
