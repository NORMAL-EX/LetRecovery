//! Offline removal boundary for the Microsoft Defender Antivirus engine.
//!
//! This module deliberately does not touch the Windows Security UI package, Firewall,
//! SmartScreen, UAC, VBS, System Guard, Web Threat Defense, Pluton, or Microsoft Defender for
//! Endpoint. The UI package is handled separately by `crate::sec_health_ui`; health/firewall
//! services remain outside both removal boundaries.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::registry::OfflineRegistry;
use crate::scoped_temp_file::{pin_existing_directory_ancestors, PinnedDirectoryAncestors};

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

#[derive(Debug)]
struct VerifiedOfflineTarget {
    root: PathBuf,
    system_ancestors: PinnedDirectoryAncestors,
    software_ancestors: PinnedDirectoryAncestors,
}

impl VerifiedOfflineTarget {
    fn verify_unchanged(&self) -> Result<()> {
        self.system_ancestors
            .verify_unchanged()
            .context("offline SYSTEM hive ancestor identity changed")?;
        self.software_ancestors
            .verify_unchanged()
            .context("offline SOFTWARE hive ancestor identity changed")?;
        Ok(())
    }
}

fn verify_offline_target_root(root: PathBuf) -> Result<VerifiedOfflineTarget> {
    let system_hive = root.join("Windows\\System32\\config\\SYSTEM");
    let software_hive = root.join("Windows\\System32\\config\\SOFTWARE");
    let system_parent = system_hive
        .parent()
        .context("offline SYSTEM hive has no parent directory")?;
    let software_parent = software_hive
        .parent()
        .context("offline SOFTWARE hive has no parent directory")?;
    let system_ancestors = pin_existing_directory_ancestors(system_parent)
        .with_context(|| format!("pin offline SYSTEM path below {}", root.display()))?;
    let software_ancestors = pin_existing_directory_ancestors(software_parent)
        .with_context(|| format!("pin offline SOFTWARE path below {}", root.display()))?;
    system_ancestors.verify_unchanged()?;
    software_ancestors.verify_unchanged()?;

    // The caller has already passed these exact files to RegLoadKeyW and all registry writes
    // below use the loaded aliases. Do not reopen a loaded hive with CreateFile here: CreateFile's
    // sharing contract makes any access incompatible with an existing opener's share mode fail,
    // and Windows legitimately keeps a loaded hive file unavailable on supported systems. Such a
    // second handle neither authenticates the loaded alias nor protects a later filesystem write.
    // Every Defender filesystem mutation separately pins its actual ancestors and target object.
    Ok(VerifiedOfflineTarget {
        root,
        system_ancestors,
        software_ancestors,
    })
}

fn normalized_target_root(target_partition: &str) -> Result<VerifiedOfflineTarget> {
    let value = target_partition.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        anyhow::bail!("target partition must be a drive letter, got {target_partition:?}");
    }
    verify_offline_target_root(PathBuf::from(format!("{}\\", value.to_ascii_uppercase())))
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
pub(crate) fn enable_file_removal_privileges() -> Result<()> {
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
pub(crate) fn enable_file_removal_privileges() -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn grant_administrators_full_control(file: &File, is_directory: bool) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SET_ACCESS,
        SE_FILE_OBJECT, TRUSTEE_IS_GROUP, TRUSTEE_IS_SID, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, WinBuiltinAdministratorsSid, CONTAINER_INHERIT_ACE,
        DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE,
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
    let result = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
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
            "SetSecurityInfo(owner) failed for retained Defender object with Win32 error {}",
            result.0
        );
    }

    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS.0,
        grfAccessMode: SET_ACCESS,
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
    let result = unsafe { SetEntriesInAclW(Some(&[access]), None, &mut acl) };
    if result != ERROR_SUCCESS {
        anyhow::bail!("SetEntriesInAclW failed with Win32 error {}", result.0);
    }
    let _acl_guard = AclGuard(acl);
    let result = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            PSID::default(),
            PSID::default(),
            Some(acl.cast_const()),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "SetSecurityInfo(DACL) failed for retained Defender object with Win32 error {}",
            result.0
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn grant_administrators_full_control(_file: &File, _is_directory: bool) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemovalIdentity {
    volume: u32,
    file: u64,
    is_directory: bool,
    has_multiple_links: bool,
}

#[cfg(windows)]
struct PinnedRemovalObject {
    file: File,
    identity: RemovalIdentity,
}

#[cfg(windows)]
struct OriginalObjectSecurity {
    descriptor: windows::Win32::Security::PSECURITY_DESCRIPTOR,
    owner: windows::Win32::Security::PSID,
    dacl: *mut windows::Win32::Security::ACL,
    dacl_protected: bool,
    canonical_owner_dacl: String,
}

#[cfg(windows)]
impl Drop for OriginalObjectSecurity {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        if !self.descriptor.0.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.descriptor.0));
            }
        }
    }
}

#[cfg(windows)]
fn security_descriptor_owner_dacl_string(
    descriptor: windows::Win32::Security::PSECURITY_DESCRIPTOR,
) -> Result<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION};

    let mut text = PWSTR::null();
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
        .context("convert retained object owner/DACL to canonical SDDL")?;
    }
    if text.is_null() {
        anyhow::bail!("security descriptor conversion returned a null string");
    }
    let value =
        unsafe { text.to_string() }.context("decode retained object owner/DACL canonical SDDL")?;
    unsafe {
        let _ = LocalFree(HLOCAL(text.0.cast()));
    }
    Ok(value)
}

#[cfg(windows)]
fn capture_object_security(file: &File) -> Result<OriginalObjectSecurity> {
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, IsValidSid,
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SE_DACL_PROTECTED,
    };

    let mut owner = PSID::default();
    let mut dacl = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let result = unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "GetSecurityInfo failed for retained hard-linked file with Win32 error {}",
            result.0
        );
    }
    let mut captured = OriginalObjectSecurity {
        descriptor,
        owner,
        dacl,
        dacl_protected: false,
        canonical_owner_dacl: String::new(),
    };
    if captured.descriptor.0.is_null()
        || captured.owner.is_invalid()
        || !unsafe { IsValidSid(captured.owner).as_bool() }
    {
        anyhow::bail!("GetSecurityInfo returned an incomplete owner/security descriptor");
    }

    let mut dacl_present = false.into();
    let mut dacl_defaulted = false.into();
    let mut descriptor_dacl = null_mut();
    unsafe {
        GetSecurityDescriptorDacl(
            captured.descriptor,
            &mut dacl_present,
            &mut descriptor_dacl,
            &mut dacl_defaulted,
        )
        .context("read retained hard-linked file DACL state")?;
    }
    if !dacl_present.as_bool() || descriptor_dacl != captured.dacl {
        anyhow::bail!("retained hard-linked file has no restorable DACL");
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    unsafe {
        GetSecurityDescriptorControl(captured.descriptor, &mut control, &mut revision)
            .context("read retained hard-linked file DACL protection state")?;
    }
    captured.dacl_protected = control & SE_DACL_PROTECTED.0 != 0;
    captured.canonical_owner_dacl = security_descriptor_owner_dacl_string(captured.descriptor)?;
    Ok(captured)
}

#[cfg(windows)]
fn restore_object_security(file: &File, original: &OriginalObjectSecurity) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSID, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    let protection = if original.dacl_protected {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    let result = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | protection,
            original.owner,
            PSID::default(),
            if original.dacl.is_null() {
                None
            } else {
                Some(original.dacl.cast_const())
            },
            None,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "restore retained hard-linked file owner/DACL failed with Win32 error {}",
            result.0
        );
    }

    verify_object_security(file, original)
}

#[cfg(windows)]
fn restore_object_owner(file: &File, original: &OriginalObjectSecurity) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSID};

    let result = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            original.owner,
            PSID::default(),
            None,
            None,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "restore retained hard-linked file owner failed with Win32 error {}",
            result.0
        );
    }
    verify_object_security(file, original)
}

#[cfg(windows)]
fn verify_object_security(file: &File, original: &OriginalObjectSecurity) -> Result<()> {
    let observed =
        capture_object_security(file).context("read back retained hard-linked file owner/DACL")?;
    if observed.canonical_owner_dacl != original.canonical_owner_dacl
        || observed.dacl_protected != original.dacl_protected
    {
        anyhow::bail!(
            "retained hard-linked file owner/DACL readback did not match its original state"
        );
    }
    Ok(())
}

#[cfg(windows)]
fn combine_primary_and_restore_error(
    primary: anyhow::Error,
    restore: anyhow::Error,
) -> anyhow::Error {
    anyhow::anyhow!("{primary}; additionally failed to restore the original hard-linked file security: {restore}")
}

#[cfg(windows)]
fn removal_identity(file: &File, allow_file_hard_links: bool) -> Result<RemovalIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        anyhow::bail!("Defender removal object is a reparse point");
    }
    let metadata = file.metadata()?;
    if information.nNumberOfLinks != 1 && !(allow_file_hard_links && metadata.is_file()) {
        anyhow::bail!(
            "Defender removal refuses an object with {} hard links",
            information.nNumberOfLinks
        );
    }
    let index = ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64;
    if index == 0 {
        anyhow::bail!("filesystem did not provide a stable Defender object file ID");
    }
    if !metadata.is_file() && !metadata.is_dir() {
        anyhow::bail!("Defender removal object is neither a file nor a directory");
    }
    Ok(RemovalIdentity {
        volume: information.dwVolumeSerialNumber,
        file: index,
        is_directory: metadata.is_dir(),
        has_multiple_links: information.nNumberOfLinks > 1,
    })
}

#[cfg(windows)]
fn open_identity_pin(
    path: &Path,
    allow_file_hard_links: bool,
) -> std::io::Result<(File, RemovalIdentity)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = std::fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = options.open(path)?;
    let identity = removal_identity(&file, allow_file_hard_links).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
    })?;
    Ok((file, identity))
}

#[cfg(windows)]
fn open_removal_object(path: &Path, allow_file_hard_links: bool) -> Result<PinnedRemovalObject> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, READ_CONTROL, WRITE_DAC,
        WRITE_OWNER,
    };

    // First retain the exact last component without delete sharing. SeTakeOwnershipPrivilege
    // grants WRITE_OWNER even when its existing DACL would deny the later DELETE/WRITE_DAC open.
    let mut owner_options = std::fs::OpenOptions::new();
    let owner_access = FILE_READ_ATTRIBUTES.0
        | WRITE_OWNER.0
        | if allow_file_hard_links {
            READ_CONTROL.0
        } else {
            0
        };
    owner_options
        .access_mode(owner_access)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let owner_handle = owner_options.open(path).with_context(|| {
        format!(
            "pin Defender object {} before taking ownership",
            path.display()
        )
    })?;
    let identity = removal_identity(&owner_handle, allow_file_hard_links)
        .with_context(|| format!("validate Defender object {}", path.display()))?;
    let original_security = if allow_file_hard_links && !identity.is_directory {
        Some(
            capture_object_security(&owner_handle)
                .with_context(|| format!("capture original security for {}", path.display()))?,
        )
    } else {
        None
    };
    set_administrators_owner(&owner_handle)?;

    // Becoming owner grants the right to rewrite the DACL, but not DELETE. Apply the narrowly
    // scoped Administrators ACE through a second handle while the original no-delete handle keeps
    // the pathname pinned, then acquire the final DELETE handle only after that ACL change.
    let mut acl_options = std::fs::OpenOptions::new();
    acl_options
        .access_mode(FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0 | WRITE_DAC.0 | WRITE_OWNER.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let acl_handle = match acl_options.open(path).with_context(|| {
        format!(
            "open retained Defender object {} for ACL update",
            path.display()
        )
    }) {
        Ok(handle) => handle,
        Err(primary) => {
            if let Some(original) = original_security.as_ref() {
                if let Err(restore) = restore_object_owner(&owner_handle, original) {
                    return Err(combine_primary_and_restore_error(primary, restore));
                }
            }
            return Err(primary);
        }
    };
    let acl_identity = match removal_identity(&acl_handle, allow_file_hard_links) {
        Ok(value) => value,
        Err(primary) => {
            if let Some(original) = original_security.as_ref() {
                if let Err(restore) = restore_object_security(&acl_handle, original) {
                    return Err(combine_primary_and_restore_error(primary, restore));
                }
            }
            return Err(primary);
        }
    };
    if acl_identity != identity {
        let primary = anyhow::anyhow!(
            "Defender object identity changed while acquiring its ACL handle: {}",
            path.display()
        );
        if let Some(original) = original_security.as_ref() {
            if let Err(restore) = restore_object_security(&acl_handle, original) {
                return Err(combine_primary_and_restore_error(primary, restore));
            }
        }
        return Err(primary);
    }
    if let Err(primary) = grant_administrators_full_control(&acl_handle, identity.is_directory) {
        if let Some(original) = original_security.as_ref() {
            if let Err(restore) = restore_object_security(&acl_handle, original) {
                return Err(combine_primary_and_restore_error(primary, restore));
            }
        }
        return Err(primary);
    }

    let mut full_options = std::fs::OpenOptions::new();
    full_options
        .access_mode(
            FILE_READ_ATTRIBUTES.0
                | FILE_WRITE_ATTRIBUTES.0
                | READ_CONTROL.0
                | WRITE_DAC.0
                | WRITE_OWNER.0
                | DELETE.0,
        )
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = match full_options.open(path).with_context(|| {
        format!(
            "open retained Defender object {} for removal",
            path.display()
        )
    }) {
        Ok(handle) => handle,
        Err(primary) => {
            if let Some(original) = original_security.as_ref() {
                if let Err(restore) = restore_object_security(&acl_handle, original) {
                    return Err(combine_primary_and_restore_error(primary, restore));
                }
            }
            return Err(primary);
        }
    };
    let full_identity = match removal_identity(&file, allow_file_hard_links) {
        Ok(value) => value,
        Err(primary) => {
            if let Some(original) = original_security.as_ref() {
                if let Err(restore) = restore_object_security(&file, original) {
                    return Err(combine_primary_and_restore_error(primary, restore));
                }
            }
            return Err(primary);
        }
    };
    if full_identity != identity {
        let primary = anyhow::anyhow!(
            "Defender object identity changed while acquiring its removal handle: {}",
            path.display()
        );
        if let Some(original) = original_security.as_ref() {
            if let Err(restore) = restore_object_security(&file, original) {
                return Err(combine_primary_and_restore_error(primary, restore));
            }
        }
        return Err(primary);
    }
    if let Some(original) = original_security.as_ref() {
        restore_object_security(&file, original)
            .with_context(|| format!("restore shared hard-link metadata for {}", path.display()))?;
    }
    drop(acl_handle);
    drop(owner_handle);
    Ok(PinnedRemovalObject { file, identity })
}

#[cfg(windows)]
fn set_administrators_owner(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        CreateWellKnownSid, WinBuiltinAdministratorsSid, OWNER_SECURITY_INFORMATION, PSID,
        SECURITY_MAX_SID_SIZE,
    };

    let mut buffer = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut length = buffer.len() as u32;
    let administrators = PSID(buffer.as_mut_ptr().cast());
    unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            PSID::default(),
            administrators,
            &mut length,
        )
        .context("CreateWellKnownSid for Defender object owner")?;
    }
    let result = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            administrators,
            PSID::default(),
            None,
            None,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "SetSecurityInfo(owner) failed for retained Defender object with Win32 error {}",
            result.0
        );
    }
    Ok(())
}

#[cfg(windows)]
fn verify_path_identity(
    path: &Path,
    expected: RemovalIdentity,
    allow_file_hard_links: bool,
) -> Result<()> {
    let (_pin, observed) = open_identity_pin(path, allow_file_hard_links)
        .with_context(|| format!("reopen Defender object identity {}", path.display()))?;
    if observed != expected {
        anyhow::bail!(
            "Defender object pathname identity changed: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn clear_read_only_attribute(path: &Path, object: &PinnedRemovalObject) -> Result<Option<u32>> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
        FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO,
    };

    let mut information = FILE_BASIC_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(object.file.as_raw_handle()),
            FileBasicInfo,
            (&mut information as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .with_context(|| format!("read attributes from retained object {}", path.display()))?;
    if information.FileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
        let original_attributes = information.FileAttributes;
        information.FileAttributes &= !FILE_ATTRIBUTE_READONLY.0;
        unsafe {
            SetFileInformationByHandle(
                HANDLE(object.file.as_raw_handle()),
                FileBasicInfo,
                (&information as *const FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        }
        .with_context(|| {
            format!(
                "clear read-only attribute through retained object handle {}",
                path.display()
            )
        })?;
        let mut observed = FILE_BASIC_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(object.file.as_raw_handle()),
                FileBasicInfo,
                (&mut observed as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        }?;
        if observed.FileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
            anyhow::bail!("read-only attribute survived on {}", path.display());
        }
        return Ok(Some(original_attributes));
    }
    Ok(None)
}

#[cfg(windows)]
fn restore_basic_information(path: &Path, file: &File, original_attributes: u32) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle, FILE_BASIC_INFO,
    };

    // FILE_BASIC_INFO documents ChangeTime as changing when file metadata changes. Replaying the
    // pre-mutation ChangeTime and then requiring it to remain byte-identical is therefore an
    // invalid compatibility gate. We changed only FILE_ATTRIBUTE_READONLY, so preserve the
    // handle's current timestamps, restore only the original attributes, and read back only that
    // property.
    let mut restored = FILE_BASIC_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            (&mut restored as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .with_context(|| {
        format!(
            "read current shared hard-link attributes for {}",
            path.display()
        )
    })?;
    restored.FileAttributes = original_attributes;
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            (&restored as *const FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .with_context(|| {
        format!(
            "restore shared hard-link file attributes for {}",
            path.display()
        )
    })?;
    let mut observed = FILE_BASIC_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            (&mut observed as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .with_context(|| {
        format!(
            "read back restored shared hard-link file attributes for {}",
            path.display()
        )
    })?;
    if observed.FileAttributes != original_attributes {
        anyhow::bail!(
            "shared hard-link file-attribute readback did not match its original state for {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn restore_basic_after_failed_delete(
    path: &Path,
    file: &File,
    original_attributes: Option<u32>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match original_attributes {
        Some(original_attributes) => {
            match restore_basic_information(path, file, original_attributes) {
                Ok(()) => primary,
                Err(restore) => anyhow::anyhow!(
                    "{primary}; additionally failed to restore the original file metadata: {restore}"
                ),
            }
        }
        None => primary,
    }
}

#[cfg(windows)]
fn set_delete_on_close(file: &File, delete: bool) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };
    let information = FILE_DISPOSITION_INFO {
        DeleteFile: BOOLEAN(u8::from(delete)),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&information as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    Ok(())
}

#[cfg(windows)]
fn remove_pinned_object(
    path: &Path,
    object: PinnedRemovalObject,
    allow_file_hard_links: bool,
) -> Result<()> {
    verify_path_identity(path, object.identity, allow_file_hard_links)?;

    if object.identity.is_directory {
        let entries = std::fs::read_dir(path)
            .with_context(|| format!("enumerate retained Defender directory {}", path.display()))?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name())
                    .with_context(|| format!("enumerate entry below {}", path.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        verify_path_identity(path, object.identity, allow_file_hard_links)?;
        for name in entries {
            if name.to_string_lossy().contains(['\\', '/']) {
                anyhow::bail!("Defender directory enumeration returned an unsafe child name");
            }
            let child_path = path.join(name);
            let child =
                open_removal_object(&child_path, allow_file_hard_links).with_context(|| {
                    format!("pin enumerated Defender child {}", child_path.display())
                })?;
            remove_pinned_object(&child_path, child, allow_file_hard_links)?;
            verify_path_identity(path, object.identity, allow_file_hard_links)?;
        }
        // Refuse to delete if a concurrent writer introduced an unenumerated child. The handle
        // disposition call returns ERROR_DIR_NOT_EMPTY and leaves that unknown object untouched.
    }

    let original_basic = clear_read_only_attribute(path, &object)?;
    if let Err(primary) = verify_path_identity(path, object.identity, allow_file_hard_links) {
        return Err(restore_basic_after_failed_delete(
            path,
            &object.file,
            original_basic,
            primary,
        ));
    }
    if let Err(primary) = set_delete_on_close(&object.file, true).with_context(|| {
        format!(
            "mark retained Defender object for deletion {}",
            path.display()
        )
    }) {
        return Err(restore_basic_after_failed_delete(
            path,
            &object.file,
            original_basic,
            primary,
        ));
    }
    if allow_file_hard_links && object.identity.has_multiple_links {
        if let Some(original_attributes) = original_basic {
            if let Err(primary) = restore_basic_information(path, &object.file, original_attributes)
            {
                let cancel = set_delete_on_close(&object.file, false)
                    .context("cancel hard-link deletion after metadata restoration failure");
                let second_restore =
                    restore_basic_information(path, &object.file, original_attributes)
                        .context("restore hard-link metadata after canceling deletion");
                return match (cancel, second_restore) {
                    (Ok(()), Ok(())) => Err(primary.context(
                        "hard-link deletion was canceled and its original metadata was restored",
                    )),
                    (Err(cancel_error), Ok(())) => Err(anyhow::anyhow!(
                        "{primary}; deletion cancellation also failed: {cancel_error}"
                    )),
                    (Ok(()), Err(restore_error)) => Err(anyhow::anyhow!(
                        "{primary}; deletion was canceled but metadata restoration still failed: {restore_error}"
                    )),
                    (Err(cancel_error), Err(restore_error)) => Err(anyhow::anyhow!(
                        "{primary}; deletion cancellation failed: {cancel_error}; metadata restoration still failed: {restore_error}"
                    )),
                };
            }
        }
    }
    drop(object);
    match open_identity_pin(path, allow_file_hard_links) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok((_replacement, identity)) => anyhow::bail!(
            "Defender pathname still exists or was reoccupied after deletion: {} (volume {}, file {})",
            path.display(), identity.volume, identity.file
        ),
        Err(error) => Err(error)
            .with_context(|| format!("verify deletion of retained Defender object {}", path.display())),
    }
}

#[cfg(windows)]
fn remove_owned_path(path: &Path) -> Result<bool> {
    remove_owned_path_with_policy(path, false)
}

#[cfg(not(windows))]
pub(crate) fn remove_owned_path_with_file_hard_links(path: &Path) -> Result<bool> {
    remove_owned_path(path)
}

#[cfg(windows)]
fn remove_owned_path_with_policy(path: &Path, allow_file_hard_links: bool) -> Result<bool> {
    let parent = path
        .parent()
        .with_context(|| format!("Defender removal path has no parent: {}", path.display()))?;
    let ancestors = pin_existing_directory_ancestors(parent)
        .with_context(|| format!("pin Defender removal ancestors for {}", path.display()))?;
    ancestors.verify_unchanged()?;
    let object = match open_removal_object(path, allow_file_hard_links) {
        Ok(object) => object,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            ancestors.verify_unchanged()?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    ancestors.verify_unchanged()?;
    remove_pinned_object(path, object, allow_file_hard_links)?;
    ancestors.verify_unchanged()?;
    Ok(true)
}

/// Remove one exact active pathname while allowing ordinary files to have other hard-link names.
///
/// Windows component-store payloads legitimately expose active System32/SysWOW64 names as hard
/// links to WinSxS. The retained-handle identity and reparse checks still protect the exact name;
/// this function deletes only that active name and deliberately leaves every other link intact.
#[cfg(windows)]
pub(crate) fn remove_owned_path_with_file_hard_links(path: &Path) -> Result<bool> {
    remove_owned_path_with_policy(path, true)
}

#[cfg(not(windows))]
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
    grant_administrators_full_control(&File::open(path)?, metadata.is_dir())?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove_owned_path(&entry?.path())?;
        }
        std::fs::remove_dir(path)?;
    } else if metadata.is_file() {
        std::fs::remove_file(path)?;
    } else {
        anyhow::bail!("refusing to remove a non-file path: {}", path.display());
    }
    Ok(true)
}

#[cfg(windows)]
fn verify_removal_path_absent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("Defender removal path has no parent: {}", path.display()))?;
    let ancestors = pin_existing_directory_ancestors(parent)?;
    ancestors.verify_unchanged()?;
    match open_identity_pin(path, false) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok((_file, identity)) => anyhow::bail!(
            "Defender engine path survived removal: {} (volume {}, file {})",
            path.display(),
            identity.volume,
            identity.file
        ),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("verify final absence of Defender path {}", path.display())
            })
        }
    }
    ancestors.verify_unchanged()?;
    Ok(())
}

#[cfg(not(windows))]
fn verify_removal_path_absent(path: &Path) -> Result<()> {
    if path.exists() {
        anyhow::bail!("Defender engine path survived removal: {}", path.display());
    }
    Ok(())
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
    let target = normalized_target_root(target_partition)?;
    target.verify_unchanged()?;
    let control_sets = discover_control_sets(system_hive_alias)?;
    let plan = DefenderRemovalPlan::new(
        target.root.clone(),
        software_hive_alias,
        system_hive_alias,
        control_sets,
    )?;

    let mut disabled_services = 0;
    for key in &plan.service_keys {
        if OfflineRegistry::key_exists(key)? {
            OfflineRegistry::set_dword(key, "Start", 4)
                .with_context(|| format!("disable Defender engine service {key}"))?;
            disabled_services += 1;
        }
    }

    let mut removed_paths = 0;
    // Enabling the two narrowly required token privileges once avoids an unsafe path-existence
    // probe before the first retained handle is acquired.
    enable_file_removal_privileges()?;
    for path in &plan.removal_paths {
        target.verify_unchanged()?;
        if !path.starts_with(&plan.target_root) {
            anyhow::bail!(
                "Defender removal path escaped target root: {}",
                path.display()
            );
        }
        if remove_owned_path(path)? {
            removed_paths += 1;
        }
        target.verify_unchanged()?;
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
        target.verify_unchanged()?;
        verify_removal_path_absent(path)?;
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

    #[cfg(windows)]
    fn temporary_test_directory(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "lr-defender-removal-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

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

    #[test]
    #[cfg(windows)]
    fn offline_target_verification_does_not_reopen_loaded_hive_files() {
        let directory = temporary_test_directory("loaded-hive-share");
        let config = directory.join("Windows\\System32\\config");
        std::fs::create_dir_all(&config).unwrap();
        let system = config.join("SYSTEM");
        let software = config.join("SOFTWARE");
        let moved_system = config.join("SYSTEM.moved");
        let moved_software = config.join("SOFTWARE.moved");
        std::fs::write(&system, b"system hive fixture").unwrap();
        std::fs::write(&software, b"software hive fixture").unwrap();

        let target = verify_offline_target_root(directory.clone()).unwrap();
        // The verifier may retain directory identity pins, but must not retain a second file
        // handle to hives which production has already loaded with RegLoadKeyW.
        std::fs::rename(&system, &moved_system).unwrap();
        std::fs::rename(&moved_system, &system).unwrap();
        std::fs::rename(&software, &moved_software).unwrap();
        std::fs::rename(&moved_software, &software).unwrap();
        target.verify_unchanged().unwrap();
        drop(target);

        std::fs::remove_file(system).unwrap();
        std::fs::remove_file(software).unwrap();
        std::fs::remove_dir(config).unwrap();
        std::fs::remove_dir(directory.join("Windows\\System32")).unwrap();
        std::fs::remove_dir(directory.join("Windows")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn retained_object_handle_blocks_path_rename_and_delete_replacement() {
        let directory = temporary_test_directory("pin");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("WdFilter.sys");
        let renamed = directory.join("replacement.sys");
        std::fs::write(&path, b"original").unwrap();

        let (pin, identity) = open_identity_pin(&path, false).unwrap();
        match std::fs::rename(&path, &renamed) {
            Err(_) => {
                verify_path_identity(&path, identity, false).unwrap();
                drop(pin);
                std::fs::rename(&path, &renamed).unwrap();
            }
            Ok(()) => {
                // Some modern filesystems permit rename while a legacy no-delete handle is
                // retained. Identity verification must still detect the missing/reoccupied name.
                assert!(verify_path_identity(&path, identity, false).is_err());
                drop(pin);
            }
        }
        std::fs::write(&path, b"attacker replacement").unwrap();
        assert!(verify_path_identity(&path, identity, false).is_err());

        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(renamed).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn reparse_child_is_rejected_before_acl_or_delete_access() {
        let directory = temporary_test_directory("reparse");
        let outside = temporary_test_directory("outside");
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("must-survive.bin"), b"outside").unwrap();
        let link = directory.join("Windows Defender");
        if std::os::windows::fs::symlink_dir(&outside, &link).is_ok() {
            let error = match open_removal_object(&link, false) {
                Ok(_) => panic!("reparse directory unexpectedly acquired a removal handle"),
                Err(error) => error,
            };
            assert!(!error.to_string().is_empty());
            assert_eq!(
                std::fs::read(outside.join("must-survive.bin")).unwrap(),
                b"outside"
            );
            std::fs::remove_dir(&link).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
        std::fs::remove_file(outside.join("must-survive.bin")).unwrap();
        std::fs::remove_dir(outside).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn multiply_linked_file_is_rejected_as_an_unknown_alias() {
        let directory = temporary_test_directory("hardlink");
        std::fs::create_dir(&directory).unwrap();
        let first = directory.join("WdBoot.sys");
        let alias = directory.join("outside-alias.sys");
        std::fs::write(&first, b"driver").unwrap();
        std::fs::hard_link(&first, &alias).unwrap();

        let error = open_identity_pin(&first, false).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let (_pin, identity) = open_identity_pin(&first, true).unwrap();
        assert!(!identity.is_directory);
        assert_eq!(std::fs::read(&alias).unwrap(), b"driver");

        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(alias).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn absent_whitelisted_path_is_a_verified_noop() {
        let directory = temporary_test_directory("absent");
        std::fs::create_dir(&directory).unwrap();
        assert!(!remove_owned_path(&directory.join("Windows Defender")).unwrap());
        std::fs::remove_dir(directory).unwrap();
    }
}
