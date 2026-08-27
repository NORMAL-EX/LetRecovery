use crate::tr;
use crate::utils::cmd::create_command;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use lr_core::cached_artifact::{
    inspect_cached_artifact, verify_cached_artifact, CachedArtifactError, CachedArtifactPresence,
    CachedArtifactStatus,
};

use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::{get_bin_dir, get_exe_dir, get_pe_download_cache_dir};

const PERSISTENT_PE_DIR_NAME: &str = "LetRecovery_PE";
const ACTIVE_PE_JOURNAL_NAME: &str = "pe_guid.txt";
const PENDING_PE_JOURNAL_NAME: &str = "pe_pending.txt";
const HANDOFF_CAPSULE_WIM_PATH: &str = "\\LR_HandoffAuth.txt";
const HANDOFF_CONFIG_WIM_PATH: &str = "\\LR_HandoffConfig.ini";
const HANDOFF_MANIFEST_WIM_PATH: &str = "\\LR_HandoffManifest.txt";
const HANDOFF_UNATTEND_WIM_PATH: &str = "\\LR_CustomUnattend.xml";
const HANDOFF_WIFI_WIM_PATH: &str = "\\LR_WifiProfile.xml";
const HANDOFF_ADMINISTRATOR_WIM_PATH: &str =
    lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_WIM_PATH;
const HANDOFF_BITLOCKER_WIM_PATH: &str = lr_core::bl_passthrough::KEYS_WIM_PATH;
const JOURNAL_VERSION: &str = "LRPE4";
const PRE_AUTH_JOURNAL_VERSION: &str = "LRPE3";
const LEGACY_JOURNAL_VERSION: &str = "LRPE2";

struct SecurePeDirectory {
    path: PathBuf,
    // Kept open without FILE_SHARE_DELETE for the whole transaction, so the validated fixed
    // directory cannot be renamed or replaced between validation and publication/rollback.
    _lock: File,
}

fn persistent_pe_directory_path() -> Result<PathBuf> {
    let drive = lr_core::windows_storage::current_windows_drive_letter()
        .context("resolve the running Windows volume for private PE staging")?;
    Ok(PathBuf::from(format!(r"{drive}:\{PERSISTENT_PE_DIR_NAME}")))
}

fn active_pe_journal(directory: &SecurePeDirectory) -> PathBuf {
    directory.path.join(ACTIVE_PE_JOURNAL_NAME)
}

fn pending_pe_journal(directory: &SecurePeDirectory) -> PathBuf {
    directory.path.join(PENDING_PE_JOURNAL_NAME)
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn open_directory_without_delete_share(path: &Path) -> std::io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

#[cfg(not(windows))]
fn open_directory_without_delete_share(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_regular_file_locked(path: &Path) -> std::io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, OPEN_EXISTING,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    let file = unsafe { File::from_raw_handle(handle.0) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "locked file is not a regular non-reparse file",
        ));
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_regular_file_locked(path: &Path) -> std::io::Result<File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "locked file is not a regular non-reparse file",
        ));
    }
    File::open(path)
}

#[cfg(windows)]
fn create_secure_directory_atomic(path: &Path) -> Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, ERROR_ALREADY_EXISTS, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;

    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 .0));
                }
            }
        }
    }

    const SDDL: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
    let sddl = SDDL.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .context("build protected PE directory security descriptor")?;
    }
    let descriptor = Descriptor(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0 .0,
        bInheritHandle: false.into(),
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    match unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&attributes)) } {
        Ok(()) => Ok(true),
        Err(_) => {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS.0 as i32) {
                Ok(false)
            } else {
                Err(error).context("atomically create protected PE directory")
            }
        }
    }
}

#[cfg(not(windows))]
fn create_secure_directory_atomic(path: &Path) -> Result<bool> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn trusted_pe_directory_sddl(value: &str) -> bool {
    let compact = value.replace(' ', "").to_ascii_uppercase();
    let owner_is_trusted = compact.starts_with("O:BA") || compact.starts_with("O:SY");
    let Some(dacl_offset) = compact.find("D:") else {
        return false;
    };
    let dacl = &compact[dacl_offset + 2..];
    let Some(first_ace) = dacl.find('(') else {
        return false;
    };
    let flags = &dacl[..first_ace];
    if !flags.contains('P') {
        return false;
    }
    let mut aces = Vec::new();
    let mut rest = &dacl[first_ace..];
    while let Some(end) = rest.find(')') {
        aces.push(&rest[..=end]);
        rest = &rest[end + 1..];
    }
    rest.is_empty()
        && owner_is_trusted
        && aces.len() == 2
        && aces.contains(&"(A;OICI;FA;;;SY)")
        && aces.contains(&"(A;OICI;FA;;;BA)")
}

#[cfg(windows)]
fn verify_secure_directory_acl(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    struct LocalPointer(*mut std::ffi::c_void);
    impl Drop for LocalPointer {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0));
                }
            }
        }
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
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        anyhow::bail!(
            "read persistent PE directory owner/DACL failed: {}",
            result.0
        );
    }
    let descriptor_guard = LocalPointer(descriptor.0);
    let mut sddl = PWSTR::null();
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut sddl,
            None,
        )
        .context("convert persistent PE directory security descriptor")?;
    }
    let sddl_guard = LocalPointer(sddl.0.cast());
    let value = unsafe { sddl.to_string() }.context("read persistent PE directory SDDL")?;
    drop(sddl_guard);
    drop(descriptor_guard);
    if !trusted_pe_directory_sddl(&value) {
        anyhow::bail!("persistent PE directory owner/DACL is not trusted: {value}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn verify_secure_directory_acl(_path: &Path) -> Result<()> {
    Ok(())
}

fn secure_pe_directory() -> Result<SecurePeDirectory> {
    let path = persistent_pe_directory_path()?;
    let root = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("persistent PE directory has no volume root"))?;
    let root_metadata = std::fs::symlink_metadata(root).context("inspect PE directory parent")?;
    if !root_metadata.is_dir() || metadata_is_reparse_point(&root_metadata) {
        anyhow::bail!("persistent PE directory parent is not a regular directory");
    }

    let mut created = create_secure_directory_atomic(&path)?;
    let mut lock = open_directory_without_delete_share(&path)
        .context("open persistent PE directory without delete sharing")?;
    let metadata = lock
        .metadata()
        .context("inspect locked persistent PE directory")?;
    if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("persistent PE directory is a reparse point or not a directory");
    }
    if let Err(untrusted) = verify_secure_directory_acl(&path) {
        let empty = std::fs::read_dir(&path)
            .context("inspect untrusted persistent PE directory")?
            .next()
            .is_none();
        if created || !empty {
            return Err(untrusted);
        }
        drop(lock);
        std::fs::remove_dir(&path).context("remove empty untrusted PE directory")?;
        created = create_secure_directory_atomic(&path)?;
        if !created {
            anyhow::bail!("persistent PE directory was recreated by another process");
        }
        lock = open_directory_without_delete_share(&path)
            .context("reopen atomically protected PE directory")?;
        verify_secure_directory_acl(&path)?;
    }
    let metadata = lock
        .metadata()
        .context("reinspect locked persistent PE directory")?;
    if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("persistent PE directory changed during ACL protection");
    }
    Ok(SecurePeDirectory { path, _lock: lock })
}

fn ensure_secure_child(directory: &SecurePeDirectory, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("persistent PE path has no parent"))?;
    if !parent
        .to_string_lossy()
        .eq_ignore_ascii_case(&directory.path.to_string_lossy())
        || path.file_name().is_none()
    {
        anyhow::bail!("persistent PE path is outside the locked directory");
    }
    Ok(())
}

fn ensure_regular_or_absent(directory: &SecurePeDirectory, path: &Path) -> Result<()> {
    ensure_secure_child(directory, path)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata_is_reparse_point(&metadata) => Ok(()),
        Ok(_) => anyhow::bail!(
            "persistent PE child is not a regular file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Debug)]
struct PeBootRecord {
    ramdisk_guid: String,
    loader_guid: String,
    wim_path: PathBuf,
    sdi_path: PathBuf,
    session_id: Option<String>,
    root_identity: Option<lr_core::install_handoff::CanonicalInstallTargetV2>,
    handoff_purpose: Option<lr_core::handoff_auth::HandoffPurpose>,
    handoff_capsule_sha256: Option<[u8; 32]>,
}

pub(crate) struct HandoffBootPayload {
    capsule: lr_core::handoff_auth::HandoffAuthCapsule,
    config_bytes: Vec<u8>,
    manifest_bytes: Vec<u8>,
    custom_unattend: Option<Vec<u8>>,
    private_wifi_profile: Option<Vec<u8>>,
    administrator_secret: Option<zeroize::Zeroizing<Vec<u8>>>,
    bitlocker_secret: Option<zeroize::Zeroizing<Vec<u8>>>,
}

impl HandoffBootPayload {
    pub(crate) fn new(
        key: lr_core::handoff_auth::SessionAuthKey,
        purpose: lr_core::handoff_auth::HandoffPurpose,
        session_id: &str,
        config_bytes: Vec<u8>,
        manifest_bytes: Vec<u8>,
        custom_unattend: Option<Vec<u8>>,
        private_wifi_profile: Option<Vec<u8>>,
    ) -> Result<Self> {
        let capsule = lr_core::handoff_auth::HandoffAuthCapsule::new(
            key,
            purpose,
            session_id,
            &config_bytes,
        )?;
        capsule.verify_config(purpose, &config_bytes)?;
        let binding = lr_core::handoff_manifest::ManifestBinding::from_config_bytes(&config_bytes)?;
        let manifest = binding.verify(&manifest_bytes)?;
        if manifest.purpose != purpose || manifest.session_id != session_id {
            anyhow::bail!("handoff manifest purpose/session does not match boot capsule");
        }
        let config_text = std::str::from_utf8(&config_bytes)
            .context("authenticated handoff config is not UTF-8")?;
        let wifi_binding =
            lr_core::first_logon::private_wifi_binding_from_install_ini(config_text)?;
        match (wifi_binding, private_wifi_profile.as_deref()) {
            (Some(binding), Some(bytes)) => binding.verify(bytes)?,
            (None, None) => {}
            _ => anyhow::bail!("private Wi-Fi profile and authenticated config disagree"),
        }
        Ok(Self {
            capsule,
            config_bytes,
            manifest_bytes,
            custom_unattend,
            private_wifi_profile,
            administrator_secret: None,
            bitlocker_secret: None,
        })
    }

    pub(crate) fn with_administrator_secret(
        mut self,
        bytes: zeroize::Zeroizing<Vec<u8>>,
    ) -> Result<Self> {
        use lr_core::handoff_manifest::{ArtifactLocation, ArtifactRole};

        lr_core::unattend_account::parse_protected_administrator_secret(&bytes)
            .map_err(anyhow::Error::msg)?;
        if self.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Install {
            anyhow::bail!("Administrator secret is valid only for PE installation");
        }
        let manifest = lr_core::handoff_manifest::HandoffManifest::parse(&self.manifest_bytes)?;
        let records = manifest
            .artifacts
            .iter()
            .filter(|record| record.role == ArtifactRole::ProtectedAdministratorSecret)
            .collect::<Vec<_>>();
        let [record] = records.as_slice() else {
            anyhow::bail!("install manifest must bind exactly one Administrator secret artifact");
        };
        let actual_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
            &lr_core::hash::sha256_bytes(&bytes),
            "protected Administrator secret SHA-256",
        )?;
        if record.location != ArtifactLocation::ProtectedBoot
            || record.relative_path
                != lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_FILE_NAME
            || record.length_bytes != bytes.len() as u64
            || record.sha256 != actual_sha256
        {
            anyhow::bail!("protected Administrator secret does not match its install manifest");
        }
        self.administrator_secret = Some(bytes);
        Ok(self)
    }

    pub(crate) fn with_bitlocker_secret(
        mut self,
        bytes: zeroize::Zeroizing<Vec<u8>>,
    ) -> Result<Self> {
        use lr_core::handoff_manifest::{ArtifactLocation, ArtifactRole};

        lr_core::bl_passthrough::parse_keys(&bytes).map_err(anyhow::Error::msg)?;
        if self.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Maintenance {
            anyhow::bail!("BitLocker recovery material is valid only for PE maintenance");
        }
        let manifest = lr_core::handoff_manifest::HandoffManifest::parse(&self.manifest_bytes)?;
        let records = manifest
            .artifacts
            .iter()
            .filter(|record| record.role == ArtifactRole::ProtectedBitLockerSecret)
            .collect::<Vec<_>>();
        let [record] = records.as_slice() else {
            anyhow::bail!("maintenance manifest must bind exactly one BitLocker secret artifact");
        };
        let actual_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
            &lr_core::hash::sha256_bytes(&bytes),
            "protected BitLocker secret SHA-256",
        )?;
        if record.location != ArtifactLocation::ProtectedBoot
            || record.relative_path != lr_core::bl_passthrough::KEYS_FILE_NAME
            || record.length_bytes != bytes.len() as u64
            || record.sha256 != actual_sha256
        {
            anyhow::bail!("protected BitLocker secret does not match its maintenance manifest");
        }
        self.bitlocker_secret = Some(bytes);
        Ok(self)
    }
}

fn inject_authenticated_handoff(
    directory: &SecurePeDirectory,
    target_wim: &Path,
    payload: &HandoffBootPayload,
) -> Result<()> {
    payload
        .capsule
        .verify_config(payload.capsule.purpose(), &payload.config_bytes)?;
    let capsule_text = payload.capsule.to_text()?;
    let (capsule_file, mut capsule_handle) =
        lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
            &directory.path,
            "handoff-capsule",
            "txt",
        )?;
    capsule_handle.write_all(capsule_text.as_bytes())?;
    capsule_handle.sync_all()?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&capsule_handle)?;

    let (config_file, mut config_handle) =
        lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
            &directory.path,
            "handoff-config",
            "ini",
        )?;
    config_handle.write_all(&payload.config_bytes)?;
    config_handle.sync_all()?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&config_handle)?;
    let (manifest_file, mut manifest_handle) =
        lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
            &directory.path,
            "handoff-manifest",
            "txt",
        )?;
    manifest_handle.write_all(&payload.manifest_bytes)?;
    manifest_handle.sync_all()?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&manifest_handle)?;
    let unattend_file = payload
        .custom_unattend
        .as_ref()
        .map(|contents| {
            let (file, mut handle) =
                lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
                    &directory.path,
                    "handoff-unattend",
                    "xml",
                )?;
            handle.write_all(contents)?;
            handle.sync_all()?;
            lr_core::scoped_temp_file::verify_system_administrators_file_custody(&handle)?;
            Ok::<_, anyhow::Error>((file, handle))
        })
        .transpose()?;
    let wifi_file = payload
        .private_wifi_profile
        .as_ref()
        .map(|contents| {
            let (file, mut handle) =
                lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
                    &directory.path,
                    "handoff-wifi",
                    "xml",
                )?;
            handle.write_all(contents)?;
            handle.sync_all()?;
            lr_core::scoped_temp_file::verify_system_administrators_file_custody(&handle)?;
            Ok::<_, anyhow::Error>((file, handle))
        })
        .transpose()?;
    let administrator_file = payload
        .administrator_secret
        .as_ref()
        .map(|contents| {
            let (file, mut handle) =
                lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
                    &directory.path,
                    "handoff-administrator",
                    "txt",
                )?;
            handle.write_all(contents)?;
            handle.sync_all()?;
            lr_core::scoped_temp_file::verify_system_administrators_file_custody(&handle)?;
            Ok::<_, anyhow::Error>((file, handle))
        })
        .transpose()?;
    let bitlocker_file = payload
        .bitlocker_secret
        .as_ref()
        .map(|contents| {
            let (file, mut handle) =
                lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
                    &directory.path,
                    "handoff-bitlocker",
                    "txt",
                )?;
            handle.write_all(contents)?;
            handle.sync_all()?;
            lr_core::scoped_temp_file::verify_system_administrators_file_custody(&handle)?;
            Ok::<_, anyhow::Error>((file, handle))
        })
        .transpose()?;
    let target_wim = target_wim
        .to_str()
        .context("private PE WIM path is not valid Unicode")?;
    let verifier = lr_core::wimlib::Wimlib::new().map_err(anyhow::Error::msg)?;
    let opened = verifier.open_wim(target_wim).map_err(anyhow::Error::msg)?;
    let info = opened
        .get_info()
        .context("read private PE WIM boot index")?;
    let boot_index = info.boot_index;
    if boot_index == 0 || boot_index > info.image_count {
        anyhow::bail!(
            "private PE WIM has invalid boot index {} for {} images",
            boot_index,
            info.image_count
        );
    }
    drop(opened);
    let manager = lr_core::wimlib::WimlibManager::new().map_err(anyhow::Error::msg)?;
    for (source, destination) in [
        (capsule_file.path(), HANDOFF_CAPSULE_WIM_PATH),
        (config_file.path(), HANDOFF_CONFIG_WIM_PATH),
        (manifest_file.path(), HANDOFF_MANIFEST_WIM_PATH),
    ] {
        manager
            .add_file_to_image(
                target_wim,
                boot_index as i32,
                source
                    .to_str()
                    .context("protected handoff temporary path is not valid Unicode")?,
                destination,
            )
            .map_err(anyhow::Error::msg)?;
    }
    if let Some((unattend_file, _unattend_handle)) = unattend_file.as_ref() {
        manager
            .add_file_to_image(
                target_wim,
                boot_index as i32,
                unattend_file
                    .path()
                    .to_str()
                    .context("protected unattend temporary path is not valid Unicode")?,
                HANDOFF_UNATTEND_WIM_PATH,
            )
            .map_err(anyhow::Error::msg)?;
    }
    if let Some((wifi_file, _wifi_handle)) = wifi_file.as_ref() {
        manager
            .add_file_to_image(
                target_wim,
                boot_index as i32,
                wifi_file
                    .path()
                    .to_str()
                    .context("protected Wi-Fi temporary path is not valid Unicode")?,
                HANDOFF_WIFI_WIM_PATH,
            )
            .map_err(anyhow::Error::msg)?;
    }
    if let Some((administrator_file, _administrator_handle)) = administrator_file.as_ref() {
        manager
            .add_file_to_image(
                target_wim,
                boot_index as i32,
                administrator_file
                    .path()
                    .to_str()
                    .context("protected Administrator temporary path is not valid Unicode")?,
                HANDOFF_ADMINISTRATOR_WIM_PATH,
            )
            .map_err(anyhow::Error::msg)?;
    }
    if let Some((bitlocker_file, _bitlocker_handle)) = bitlocker_file.as_ref() {
        manager
            .add_file_to_image(
                target_wim,
                boot_index as i32,
                bitlocker_file
                    .path()
                    .to_str()
                    .context("protected BitLocker temporary path is not valid Unicode")?,
                HANDOFF_BITLOCKER_WIM_PATH,
            )
            .map_err(anyhow::Error::msg)?;
    }

    let verification =
        lr_core::scoped_temp_file::ScopedTempDir::create_in(&directory.path, "handoff-readback")?;
    let mut paths = vec![
        HANDOFF_CAPSULE_WIM_PATH,
        HANDOFF_CONFIG_WIM_PATH,
        HANDOFF_MANIFEST_WIM_PATH,
    ];
    if unattend_file.is_some() {
        paths.push(HANDOFF_UNATTEND_WIM_PATH);
    }
    if wifi_file.is_some() {
        paths.push(HANDOFF_WIFI_WIM_PATH);
    }
    if administrator_file.is_some() {
        paths.push(HANDOFF_ADMINISTRATOR_WIM_PATH);
    }
    if bitlocker_file.is_some() {
        paths.push(HANDOFF_BITLOCKER_WIM_PATH);
    }
    manager
        .extract_paths(
            target_wim,
            boot_index,
            verification.path().to_string_lossy().as_ref(),
            &paths,
        )
        .map_err(anyhow::Error::msg)?;
    let actual_capsule = lr_core::scoped_temp_file::read_bounded_plain_file(
        &verification.path().join("LR_HandoffAuth.txt"),
        lr_core::handoff_auth::AUTH_CAPSULE_MAX_BYTES as u64,
    )?;
    let actual_config = lr_core::scoped_temp_file::read_bounded_plain_file(
        &verification.path().join("LR_HandoffConfig.ini"),
        lr_core::handoff_auth::AUTH_CONFIG_MAX_BYTES as u64,
    )?;
    let actual_manifest = lr_core::scoped_temp_file::read_bounded_plain_file(
        &verification.path().join("LR_HandoffManifest.txt"),
        lr_core::handoff_manifest::HANDOFF_MANIFEST_MAX_BYTES as u64,
    )?;
    if actual_capsule != capsule_text.as_bytes()
        || actual_config != payload.config_bytes
        || actual_manifest != payload.manifest_bytes
    {
        anyhow::bail!("authenticated PE handoff readback does not match injected bytes");
    }
    if let Some(expected) = payload.custom_unattend.as_ref() {
        let actual = lr_core::scoped_temp_file::read_bounded_plain_file(
            &verification.path().join("LR_CustomUnattend.xml"),
            lr_core::handoff_auth::AUTH_CONFIG_MAX_BYTES as u64,
        )?;
        if &actual != expected {
            anyhow::bail!("protected custom unattend readback does not match injected bytes");
        }
    }
    if let Some(expected) = payload.private_wifi_profile.as_ref() {
        let actual = lr_core::scoped_temp_file::read_bounded_plain_file(
            &verification.path().join("LR_WifiProfile.xml"),
            lr_core::first_logon::PRIVATE_WIFI_PROFILE_MAX_BYTES,
        )?;
        if &actual != expected {
            anyhow::bail!("protected Wi-Fi readback does not match injected bytes");
        }
    }
    if let Some(expected) = payload.administrator_secret.as_ref() {
        let actual = lr_core::scoped_temp_file::read_bounded_plain_file(
            &verification
                .path()
                .join(lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_FILE_NAME),
            lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_MAX_BYTES,
        )?;
        if actual.as_slice() != expected.as_slice() {
            anyhow::bail!("protected Administrator secret readback does not match injected bytes");
        }
    }
    if let Some(expected) = payload.bitlocker_secret.as_ref() {
        let actual = lr_core::scoped_temp_file::read_bounded_plain_file(
            &verification
                .path()
                .join(lr_core::bl_passthrough::KEYS_FILE_NAME),
            lr_core::bl_passthrough::MAX_BUNDLE_BYTES,
        )?;
        if actual.as_slice() != expected.as_slice() {
            anyhow::bail!("protected BitLocker secret readback does not match injected bytes");
        }
    }
    verifier
        .open_wim(target_wim)
        .map_err(anyhow::Error::msg)?
        .verify()
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

fn valid_bcd_guid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 38
        && bytes.first() == Some(&b'{')
        && bytes.last() == Some(&b'}')
        && bytes[9] == b'-'
        && bytes[14] == b'-'
        && bytes[19] == b'-'
        && bytes[24] == b'-'
        && bytes[1..37]
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}

fn fresh_bcd_guid() -> Result<String> {
    let guid = windows::core::GUID::new().context("generate BCD object GUID")?;
    Ok(format!("{{{guid:?}}}"))
}

fn bcd_path_on_volume(path: &Path, drive: char) -> Result<String> {
    let value = path.to_string_lossy().replace('/', "\\");
    let bytes = value.as_bytes();
    if bytes.len() < 3
        || bytes[1] != b':'
        || !bytes[0].is_ascii_alphabetic()
        || !(bytes[0] as char).eq_ignore_ascii_case(&drive)
        || bytes[2] != b'\\'
    {
        anyhow::bail!(
            "private PE payload is not rooted on the running Windows volume: {}",
            path.display()
        );
    }
    Ok(value[2..].to_owned())
}

fn validate_session_payload(
    persistent_directory: &Path,
    path: &Path,
    extension: &str,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("PE payload path has no parent"))?;
    if !parent
        .to_string_lossy()
        .eq_ignore_ascii_case(&persistent_directory.to_string_lossy())
    {
        anyhow::bail!("PE payload is outside the persistent PE directory");
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("PE payload filename is not valid Unicode"))?;
    let legacy_name = match extension {
        ".wim" => "boot.wim",
        ".sdi" => "boot.sdi",
        _ => "",
    };
    if name != legacy_name && (!name.starts_with("boot-") || !name.ends_with(extension)) {
        anyhow::bail!("PE payload filename is not session-scoped");
    }
    Ok(())
}

fn serialize_boot_records(
    persistent_directory: &Path,
    records: &[PeBootRecord],
) -> Result<Vec<u8>> {
    let mut output = String::new();
    for record in records {
        if !valid_bcd_guid(&record.ramdisk_guid) || !valid_bcd_guid(&record.loader_guid) {
            anyhow::bail!("refusing to serialize an invalid PE BCD GUID");
        }
        validate_session_payload(persistent_directory, &record.wim_path, ".wim")?;
        validate_session_payload(persistent_directory, &record.sdi_path, ".sdi")?;
        let authenticated = record
            .session_id
            .as_ref()
            .zip(record.root_identity.as_ref())
            .zip(record.handoff_purpose)
            .zip(record.handoff_capsule_sha256.as_ref());
        let has_any_authenticated_field = record.session_id.is_some()
            || record.root_identity.is_some()
            || record.handoff_purpose.is_some()
            || record.handoff_capsule_sha256.is_some();
        if has_any_authenticated_field && authenticated.is_none() {
            anyhow::bail!("LRPE4 handoff fields must be present together");
        }
        output.push_str(if authenticated.is_some() {
            JOURNAL_VERSION
        } else {
            LEGACY_JOURNAL_VERSION
        });
        output.push('\t');
        output.push_str(&record.ramdisk_guid);
        output.push('\t');
        output.push_str(&record.loader_guid);
        output.push('\t');
        output.push_str(&record.wim_path.to_string_lossy());
        output.push('\t');
        output.push_str(&record.sdi_path.to_string_lossy());
        if let Some((((session_id, identity), purpose), capsule_sha256)) = authenticated {
            if session_id.is_empty()
                || session_id.len() > 64
                || !session_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            {
                anyhow::bail!("invalid PE boot journal SessionId");
            }
            output.push('\t');
            output.push_str(session_id);
            output.push('\t');
            output.push_str(&lr_core::install_handoff::encode_hex(
                &identity.layout_digest,
            ));
            output.push('\t');
            output.push_str(&identity.partition_offset_bytes.to_string());
            output.push('\t');
            output.push_str(&identity.partition_length_bytes.to_string());
            output.push('\t');
            output.push_str(identity.style.as_str());
            output.push('\t');
            output.push_str(
                &identity
                    .gpt_partition_id
                    .as_ref()
                    .map(|value| lr_core::install_handoff::encode_hex(value))
                    .unwrap_or_else(|| "none".to_string()),
            );
            output.push('\t');
            output.push_str(
                &identity
                    .device_id_hash
                    .as_ref()
                    .map(|value| lr_core::install_handoff::encode_hex(value))
                    .unwrap_or_else(|| "none".to_string()),
            );
            output.push('\t');
            output.push_str(purpose.as_str());
            output.push('\t');
            output.push_str(&lr_core::install_handoff::encode_hex(capsule_sha256));
        }
        output.push_str("\r\n");
    }
    Ok(output.into_bytes())
}

fn parse_boot_records(persistent_directory: &Path, contents: &str) -> Result<Vec<PeBootRecord>> {
    let lines: Vec<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return Ok(Vec::new());
    }

    // Compatibility with the historical two-GUID journal. Its payload used fixed paths.
    if lines.len() == 2 && lines.iter().all(|line| valid_bcd_guid(line)) {
        return Ok(vec![PeBootRecord {
            ramdisk_guid: lines[0].to_string(),
            loader_guid: lines[1].to_string(),
            wim_path: persistent_directory.join("boot.wim"),
            sdi_path: persistent_directory.join("boot.sdi"),
            session_id: None,
            root_identity: None,
            handoff_purpose: None,
            handoff_capsule_sha256: None,
        }]);
    }

    lines
        .into_iter()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if !((fields.len() == 5 && fields[0] == LEGACY_JOURNAL_VERSION)
                || (fields.len() == 12 && fields[0] == PRE_AUTH_JOURNAL_VERSION)
                || (fields.len() == 14 && fields[0] == JOURNAL_VERSION))
            {
                anyhow::bail!("invalid PE BCD journal record");
            }
            if !valid_bcd_guid(fields[1]) || !valid_bcd_guid(fields[2]) {
                anyhow::bail!("invalid GUID in PE BCD journal");
            }
            let record = PeBootRecord {
                ramdisk_guid: fields[1].to_string(),
                loader_guid: fields[2].to_string(),
                wim_path: PathBuf::from(fields[3]),
                sdi_path: PathBuf::from(fields[4]),
                session_id: matches!(fields[0], JOURNAL_VERSION | PRE_AUTH_JOURNAL_VERSION)
                    .then(|| fields[5].to_string()),
                root_identity: if matches!(fields[0], JOURNAL_VERSION | PRE_AUTH_JOURNAL_VERSION) {
                    Some(
                        lr_core::install_handoff::canonical_target_from_fields(
                            Some(lr_core::install_handoff::CANONICAL_TARGET_VERSION),
                            Some(fields[6]),
                            Some(fields[7].parse().context("parse PE journal root offset")?),
                            Some(fields[8].parse().context("parse PE journal root length")?),
                            Some(fields[9]),
                            Some(fields[10]),
                            (fields[11] != "none").then_some(fields[11]),
                        )?
                        .context("PE journal root identity is missing")?,
                    )
                } else {
                    None
                },
                handoff_purpose: if fields[0] == JOURNAL_VERSION {
                    Some(lr_core::handoff_auth::HandoffPurpose::parse(fields[12])?)
                } else {
                    None
                },
                handoff_capsule_sha256: if fields[0] == JOURNAL_VERSION {
                    Some(lr_core::install_handoff::decode_hex_array::<32>(
                        fields[13],
                        "PE handoff capsule SHA-256",
                    )?)
                } else {
                    None
                },
            };
            validate_session_payload(persistent_directory, &record.wim_path, ".wim")?;
            validate_session_payload(persistent_directory, &record.sdi_path, ".sdi")?;
            Ok(record)
        })
        .collect()
}

fn write_journal(
    directory: &SecurePeDirectory,
    path: &Path,
    records: &[PeBootRecord],
) -> Result<()> {
    let bytes = serialize_boot_records(&directory.path, records)?;
    write_journal_bytes(directory, path, &bytes)
}

fn write_journal_bytes(directory: &SecurePeDirectory, path: &Path, bytes: &[u8]) -> Result<()> {
    ensure_regular_or_absent(directory, path)?;
    let (temporary, mut temporary_handle) =
        lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
            &directory.path,
            "pe-bcd-journal",
            "tmp",
        )?;
    temporary_handle.write_all(bytes)?;
    temporary_handle.sync_all()?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&temporary_handle)?;
    drop(temporary_handle);
    temporary.persist_replace(path)?;
    let mut actual = Vec::new();
    use std::io::Read as _;
    let mut published = open_regular_file_locked(path)?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&published)
        .context("verify published PE BCD journal custody")?;
    published.read_to_end(&mut actual)?;
    if actual != bytes {
        anyhow::bail!("PE BCD journal read-back differs after publish");
    }
    Ok(())
}

fn remove_file_verified(directory: &SecurePeDirectory, path: &Path) -> Result<()> {
    ensure_regular_or_absent(directory, path)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => anyhow::bail!("file remains after removal: {}", path.display()),
    }
    Ok(())
}

fn copy_file_atomic_observed(
    directory: &SecurePeDirectory,
    source: &Path,
    destination: &Path,
) -> Result<(u64, String)> {
    ensure_regular_or_absent(directory, destination)?;
    let mut source_file = open_regular_file_locked(source)
        .with_context(|| format!("lock PE payload source {}", source.display()))?;
    let expected = source_file.metadata()?.len();
    if expected == 0 {
        anyhow::bail!("PE payload source is empty: {}", source.display());
    }
    let (temporary, mut temporary_file) =
        lr_core::scoped_temp_file::ScopedTempFile::create_system_administrators_writer_in(
            &directory.path,
            "pe-payload",
            "tmp",
        )?;
    let (copied, source_sha256) =
        lr_core::hash::copy_and_sha256(&mut source_file, &mut temporary_file, |_| Ok(()))?;
    temporary_file.sync_all()?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&temporary_file)?;
    drop(temporary_file);
    if copied != expected || std::fs::metadata(temporary.path())?.len() != expected {
        anyhow::bail!("PE payload copy size differs from source");
    }
    let temporary_sha256 =
        lr_core::hash::sha256_reader(open_regular_file_locked(temporary.path())?, |_| {})?;
    if temporary_sha256 != source_sha256 {
        anyhow::bail!("PE payload staging SHA-256 differs from locked source");
    }
    temporary.persist_replace(destination)?;
    let destination_file = open_regular_file_locked(destination)?;
    lr_core::scoped_temp_file::verify_system_administrators_file_custody(&destination_file)
        .context("verify published private PE payload custody")?;
    if destination_file.metadata()?.len() != expected {
        anyhow::bail!("PE payload read-back size differs after publish");
    }
    let destination_sha256 = lr_core::hash::sha256_reader(destination_file, |_| {})?;
    if destination_sha256 != source_sha256 {
        anyhow::bail!("PE payload read-back SHA-256 differs after publish");
    }
    Ok((expected, source_sha256))
}

fn copy_file_atomic(
    directory: &SecurePeDirectory,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    copy_file_atomic_observed(directory, source, destination).map(|_| ())
}

/// A private, locked snapshot of the local PE selected by a caller.
///
/// The directory handle denies delete sharing until this value is dropped. The source is copied
/// from a regular non-reparse handle which denies both write and delete sharing; all later PE
/// preparation must consume `path`, never reopen the original cache path. This copy boundary does
/// not compare the local WIM with catalogue hashes: users may intentionally replace or customize
/// any already-downloaded PE. Catalogue integrity is enforced only while downloading the file.
pub(crate) struct LocalPeSnapshot {
    pub(crate) path: PathBuf,
    // Field order matters: deny child writes/deletes until all consumers are done, then release
    // the directory lock before the recursive temporary-directory cleanup guard runs.
    _file_lock: File,
    _directory_lock: File,
    _directory: lr_core::scoped_temp_file::ScopedTempDir,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PeBootPurpose {
    Install,
    Backup,
    Maintenance,
}

impl PeBootPurpose {
    const fn may_inject_bitlocker_recovery_material(self) -> bool {
        matches!(self, Self::Maintenance)
    }
}

pub(crate) fn snapshot_local_pe(source: &Path, filename: &str) -> Result<LocalPeSnapshot> {
    lr_core::download_integrity::validate_download_filename(filename)
        .map_err(|error| anyhow::anyhow!("invalid PE snapshot filename: {error}"))?;

    let directory = lr_core::scoped_temp_file::ScopedTempDir::create_in(
        &std::env::temp_dir(),
        "letrecovery-pe-snapshot",
    )?;
    lr_core::scoped_temp_file::restrict_to_current_user_system_and_administrators(directory.path())
        .context("protect private PE snapshot directory")?;
    let lock = open_directory_without_delete_share(directory.path())
        .context("lock private PE snapshot directory")?;
    let secure = SecurePeDirectory {
        path: directory.path().to_path_buf(),
        _lock: lock,
    };
    let snapshot_path = secure.path.join(filename);
    let (copied_size, copied_sha256) = copy_file_atomic_observed(&secure, source, &snapshot_path)?;

    let mut readback = open_regular_file_locked(&snapshot_path)?;
    let readback_size = readback.metadata()?.len();
    let readback_sha256 = lr_core::hash::sha256_reader(&mut readback, |_| {})?;
    if readback_size != copied_size || !readback_sha256.eq_ignore_ascii_case(&copied_sha256) {
        anyhow::bail!("private PE snapshot does not match the locked source bytes");
    }

    Ok(LocalPeSnapshot {
        path: snapshot_path,
        _file_lock: readback,
        _directory_lock: secure._lock,
        _directory: directory,
    })
}

fn ensure_bcdedit_success(
    arguments: &[&str],
    success: bool,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    if success {
        return Ok(());
    }
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    anyhow::bail!(
        "{}",
        tr!(
            "bcdedit 执行失败（参数：{}，退出码：{}）：{}",
            arguments.join(" "),
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| tr!("未知")),
            detail
        )
    )
}

fn copy_first_boot_sdi(
    directory: &SecurePeDirectory,
    target: &Path,
    candidates: &[PathBuf],
) -> Result<PathBuf> {
    for source in candidates {
        if !source.is_file() {
            continue;
        }
        let expected_size = std::fs::metadata(source)?.len();
        if expected_size == 0 {
            anyhow::bail!("{}", tr!("boot.sdi 文件为空：{}", source.display()));
        }
        copy_file_atomic(directory, source, target)?;
        let copied = std::fs::metadata(target)?.len();
        let actual_size = std::fs::metadata(target)?.len();
        if copied != expected_size || actual_size != expected_size {
            anyhow::bail!(
                "{}",
                tr!(
                    "boot.sdi 复制后大小不一致：源 {} 字节，目标 {} 字节",
                    expected_size,
                    actual_size
                )
            );
        }
        return Ok(target.to_path_buf());
    }
    anyhow::bail!(
        "{}",
        tr!(
            "未找到可信的 boot.sdi，已停止创建 PE 引导；请修复当前 Windows 启动文件或使用包含 boot.sdi 的 PE ISO"
        )
    )
}

#[cfg(feature = "ci-automation")]
fn ci_session_fault_value(value: &str, name: &str) -> bool {
    let Some(run_id) = value
        .strip_prefix(name)
        .and_then(|suffix| suffix.strip_prefix(':'))
    else {
        return false;
    };
    run_id.len() == 32 && run_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(feature = "ci-automation")]
fn ci_missing_boot_sdi_fault_requested() -> bool {
    std::env::var("LETRECOVERY_CI_FAULT")
        .is_ok_and(|value| ci_session_fault_value(&value, "missing_boot_sdi"))
}

#[cfg(feature = "ci-automation")]
pub(super) fn ci_after_auto_staging_fault_requested() -> bool {
    std::env::var("LETRECOVERY_CI_FAULT")
        .is_ok_and(|value| ci_session_fault_value(&value, "after_auto_staging"))
}

#[cfg(feature = "ci-automation")]
pub(super) fn ci_force_auto_staging_requested() -> bool {
    std::env::var("LETRECOVERY_CI_FAULT").is_ok_and(|value| {
        ci_session_fault_value(&value, "after_auto_staging")
            || ci_session_fault_value(&value, "before_target_write")
    }) || std::env::var("LETRECOVERY_CI_FORCE_AUTO_STAGING")
        .is_ok_and(|value| ci_session_fault_value(&value, "auto_staging"))
}

/// WinPE 启动管理器
trait BcdRunner {
    fn run(&self, arguments: &[&str]) -> Result<String>;
}

fn output_contains_guid(output: &str, guid: &str) -> bool {
    output
        .to_ascii_lowercase()
        .contains(&guid.to_ascii_lowercase())
}

fn bcd_object_is_present<R: BcdRunner>(runner: &R, guid: &str) -> Result<bool> {
    let lookup = runner.run(&["/enum", guid, "/v"]);
    match lookup {
        Ok(output) if output_contains_guid(&output, guid) => Ok(true),
        Ok(_) => {
            // Microsoft documents `/enum <id> /v` as listing that exact object, but BCDEdit can
            // return exit code 0 with a localized "no matching objects" message. The complete
            // identifier in stdout is therefore the object-presence proof; an independent
            // Boot Manager query proves the store itself is still readable without parsing text.
            runner.run(&["/enum", "{bootmgr}", "/v"]).context(
                "verify that the BCD store remains readable after an empty object query",
            )?;
            Ok(false)
        }
        Err(missing_or_store_error) => {
            runner
                .run(&["/enum", "{bootmgr}", "/v"])
                .with_context(|| {
                    format!(
                        "cannot distinguish an absent BCD object {guid} from an unreadable BCD store; original error: {missing_or_store_error}"
                    )
                })?;
            Ok(false)
        }
    }
}

fn delete_bcd_object_with_readback<R: BcdRunner>(runner: &R, guid: &str) -> Result<()> {
    if !bcd_object_is_present(runner, guid)? {
        log::info!("[PE] BCD object was already absent: {guid}");
        return Ok(());
    }

    let deletion = runner.run(&["/delete", guid]);
    if bcd_object_is_present(runner, guid)? {
        return match deletion {
            Ok(_) => Err(anyhow::anyhow!(
                "BCD object remains after successful removal: {guid}"
            )),
            Err(error) => Err(anyhow::anyhow!(
                "delete BCD object {guid} failed and the object remains: {error}"
            )),
        };
    }

    // A failed delete can race with an earlier interrupted cleanup. Fresh enumeration is the
    // authoritative idempotent postcondition, just as for BootSequence removal above.
    log::info!("[PE] BCD object is absent after rollback: {guid}");
    Ok(())
}

fn add_one_shot_boot<R: BcdRunner>(runner: &R, loader_guid: &str) -> Result<()> {
    if !valid_bcd_guid(loader_guid) {
        anyhow::bail!("invalid PE loader GUID before setting one-shot boot sequence");
    }
    runner.run(&["/bootsequence", loader_guid, "/addfirst"])?;
    let bootmgr = runner.run(&["/enum", "{bootmgr}"])?;
    if !output_contains_guid(&bootmgr, loader_guid) {
        anyhow::bail!("PE loader is absent from BootSequence after bcdedit reported success");
    }
    Ok(())
}

fn remove_one_shot_boot<R: BcdRunner>(runner: &R, loader_guid: &str) -> Result<()> {
    let removal = runner.run(&["/bootsequence", loader_guid, "/remove"]);
    let bootmgr = runner
        .run(&["/enum", "{bootmgr}"])
        .context("read back BootSequence after rollback")?;
    if output_contains_guid(&bootmgr, loader_guid) {
        return match removal {
            Ok(_) => Err(anyhow::anyhow!(
                "PE loader remains in BootSequence after successful remove"
            )),
            Err(error) => Err(anyhow::anyhow!(
                "remove PE loader from BootSequence: {error}"
            )),
        };
    }
    // `/remove` can fail when an interrupted `/create` never made the object visible. The
    // independent Boot Manager read-back is the authoritative safe postcondition.
    Ok(())
}

#[must_use = "the armed PE boot transaction must be explicitly committed or rolled back"]
pub(crate) struct PeBootTransaction {
    manager: PeManager,
    directory: SecurePeDirectory,
    record: PeBootRecord,
    active: bool,
}

impl PeBootTransaction {
    pub(crate) fn commit(mut self) -> Result<()> {
        self.manager
            .commit_boot_record(&self.directory, &self.record)?;
        self.active = false;
        if let Err(error) =
            remove_file_verified(&self.directory, &pending_pe_journal(&self.directory))
        {
            log::error!("failed to remove committed PE pending journal: {error}");
        }
        Ok(())
    }

    pub(crate) fn rollback(mut self) -> Result<()> {
        let result = self
            .manager
            .rollback_boot_record(&self.directory, &self.record);
        let journal_result =
            remove_file_verified(&self.directory, &pending_pe_journal(&self.directory));
        self.active = false;
        match (result, journal_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(first), Err(second)) => Err(anyhow::anyhow!("{first}; {second}")),
        }
    }
}

impl Drop for PeBootTransaction {
    fn drop(&mut self) {
        if self.active {
            if let Err(error) = self
                .manager
                .rollback_boot_record(&self.directory, &self.record)
            {
                log::error!("failed to roll back dropped PE BCD transaction: {error}");
            }
            if let Err(error) =
                remove_file_verified(&self.directory, &pending_pe_journal(&self.directory))
            {
                log::error!("failed to remove dropped PE pending journal: {error}");
            }
            self.active = false;
        }
    }
}

#[derive(Clone)]
pub struct PeManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

impl BcdRunner for PeManager {
    fn run(&self, arguments: &[&str]) -> Result<String> {
        self.run_bcdedit(arguments)
    }
}

impl PeManager {
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        let bcdedit_path = lr_core::windows_compat::system_directory()
            .map(|directory| directory.join("bcdedit.exe"))
            .unwrap_or_else(|error| {
                log::error!("[PE BOOT] 无法解析宿主 System32，bcdedit 将失败关闭: {error}");
                PathBuf::from("__LetRecovery_missing_System32__").join("bcdedit.exe")
            });
        Self {
            bcdedit_path: bcdedit_path.to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    fn user_managed_directories() -> Vec<PathBuf> {
        let exe_dir = get_exe_dir();
        vec![
            get_bin_dir().join("pe"),
            exe_dir.clone(),
            exe_dir.join("PE"),
            exe_dir.join("pe"),
        ]
    }

    fn managed_cache_directories() -> Vec<PathBuf> {
        let mut directories = vec![get_pe_download_cache_dir()];
        if let Some(download_dir) = dirs::download_dir() {
            directories.push(download_dir);
        }
        directories
    }

    /// Locate a user-managed local PE or a previously downloaded PE.
    ///
    /// Every already-local PE intentionally remains customizable and is constrained only to a
    /// regular, non-reparse file. Server metadata is consumed by the download executor and must
    /// not be reused as a launch-time gate after a user has replaced or customized the WIM.
    pub fn find_cached_pe(
        filename: &str,
        _sha256: Option<&str>,
        _md5: Option<&str>,
    ) -> std::result::Result<CachedArtifactPresence, CachedArtifactError> {
        inspect_pe_candidates(
            filename,
            &Self::user_managed_directories(),
            &Self::managed_cache_directories(),
            None,
            None,
        )
    }

    /// 查找可使用的本地 PE 文件。
    ///
    /// 文件名来自服务器配置，因此在拼接路径前必须先通过单文件名校验。
    /// 无论文件位于随包目录还是下载缓存，启动时都允许用户自定义，不再与目录中的
    /// SHA-256/MD5 比较；哈希校验只发生在下载完成、文件尚未发布为缓存的边界。
    pub fn check_cached_pe(
        filename: &str,
        _sha256: Option<&str>,
        _md5: Option<&str>,
    ) -> std::result::Result<CachedArtifactStatus, CachedArtifactError> {
        verify_pe_candidates(
            filename,
            &Self::user_managed_directories(),
            &Self::managed_cache_directories(),
            None,
            None,
        )
    }

    /// 使用共享 WinAPI 边界检查当前 Windows 的实际固件启动模式。
    pub fn is_uefi_boot() -> Result<bool> {
        match lr_core::windows_firmware::detect_firmware_type()? {
            lr_core::windows_firmware::FirmwareType::Uefi => Ok(true),
            lr_core::windows_firmware::FirmwareType::Bios => Ok(false),
        }
    }

    pub(crate) fn boot_to_pe_for_install(
        &self,
        pe_path: &str,
        display_name: &str,
        payload: HandoffBootPayload,
    ) -> Result<PeBootTransaction> {
        if payload.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Install {
            anyhow::bail!("install PE boot received a non-install authentication capsule");
        }
        self.boot_to_pe_internal(pe_path, display_name, Some(payload), PeBootPurpose::Install)
    }

    /// Prepare the PE used by an LRBK2 backup. Backup authorization requires both data volumes to
    /// be unencrypted, so recovery material must never be collected or injected into this WIM.
    pub(crate) fn boot_to_pe_for_backup(
        &self,
        pe_path: &str,
        display_name: &str,
        payload: HandoffBootPayload,
    ) -> Result<PeBootTransaction> {
        if payload.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Backup {
            anyhow::bail!("backup PE boot received a non-backup authentication capsule");
        }
        self.boot_to_pe_internal(pe_path, display_name, Some(payload), PeBootPurpose::Backup)
    }

    pub(crate) fn boot_to_pe_for_expand(
        &self,
        pe_path: &str,
        display_name: &str,
        payload: HandoffBootPayload,
    ) -> Result<PeBootTransaction> {
        if payload.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Expand {
            anyhow::bail!("expand PE boot received a non-expand authentication capsule");
        }
        self.boot_to_pe_internal(pe_path, display_name, Some(payload), PeBootPurpose::Install)
    }

    pub(crate) fn boot_to_pe_for_maintenance(
        &self,
        pe_path: &str,
        display_name: &str,
        payload: HandoffBootPayload,
    ) -> Result<PeBootTransaction> {
        if payload.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Maintenance {
            anyhow::bail!("maintenance PE boot received a non-maintenance authentication capsule");
        }
        self.boot_to_pe_internal(
            pe_path,
            display_name,
            Some(payload),
            PeBootPurpose::Maintenance,
        )
    }

    fn boot_to_pe_internal(
        &self,
        pe_path: &str,
        display_name: &str,
        authenticated_handoff: Option<HandoffBootPayload>,
        purpose: PeBootPurpose,
    ) -> Result<PeBootTransaction> {
        log::info!("[PE] ========== 准备启动 PE ==========");
        log::info!("[PE] PE文件: {}", pe_path);
        log::info!("[PE] 显示名称: {}", display_name);

        let pe_path_lower = pe_path.to_lowercase();

        let directory = secure_pe_directory()?;
        self.recover_pending_boot_transaction(&directory)?;
        self.cleanup_old_pe_entries(&directory)?;
        self.cleanup_orphaned_private_pe_files(&directory)?;
        let ramdisk_guid = fresh_bcd_guid()?;
        let loader_guid = fresh_bcd_guid()?;
        let token = loader_guid
            .trim_matches(['{', '}'])
            .replace('-', "")
            .to_ascii_lowercase();
        let capsule_text = authenticated_handoff
            .as_ref()
            .map(|payload| payload.capsule.to_text())
            .transpose()?;
        let capsule_sha256 = capsule_text
            .as_ref()
            .map(|text| {
                lr_core::install_handoff::decode_hex_array::<32>(
                    &lr_core::hash::sha256_bytes(text.as_bytes()),
                    "PE handoff capsule SHA-256",
                )
            })
            .transpose()?;
        let record = PeBootRecord {
            ramdisk_guid,
            loader_guid,
            wim_path: directory.path.join(format!("boot-{token}.wim")),
            sdi_path: directory.path.join(format!("boot-{token}.sdi")),
            session_id: authenticated_handoff
                .as_ref()
                .map(|payload| payload.capsule.session_id().to_string()),
            root_identity: if authenticated_handoff.is_some() {
                let system_drive = lr_core::windows_storage::current_windows_drive_letter()
                    .context("resolve running Windows volume before PE handoff")?;
                let stable = lr_core::windows_storage::stable_volume_identity(system_drive)
                    .context("capture persistent PE root volume identity")?;
                let snapshot =
                    lr_core::windows_storage::disk_layout_snapshot(stable.extent.disk_number)
                        .context("capture persistent PE root disk layout")?;
                Some(
                    lr_core::install_handoff::CanonicalInstallTargetV2::from_snapshot(
                        &snapshot,
                        stable.extent.offset_bytes,
                        stable.extent.extent_length_bytes,
                    )?,
                )
            } else {
                None
            },
            handoff_purpose: authenticated_handoff
                .as_ref()
                .map(|payload| payload.capsule.purpose()),
            handoff_capsule_sha256: capsule_sha256,
        };
        write_journal(
            &directory,
            &pending_pe_journal(&directory),
            std::slice::from_ref(&record),
        )?;
        let transaction = PeBootTransaction {
            manager: self.clone(),
            directory,
            record,
            active: true,
        };

        if pe_path_lower.ends_with(".iso") {
            self.boot_from_iso(
                &transaction.directory,
                pe_path,
                display_name,
                &transaction.record,
                purpose,
                authenticated_handoff.as_ref(),
            )?;
        } else if pe_path_lower.ends_with(".wim") {
            self.boot_from_wim(
                &transaction.directory,
                pe_path,
                display_name,
                &transaction.record,
                purpose,
                authenticated_handoff.as_ref(),
            )?;
        } else {
            anyhow::bail!("{}", tr!("不支持的PE文件格式，请使用 .iso 或 .wim 文件"));
        }
        Ok(transaction)
    }

    /// 从ISO启动PE
    fn boot_from_iso(
        &self,
        directory: &SecurePeDirectory,
        iso_path: &str,
        display_name: &str,
        record: &PeBootRecord,
        _purpose: PeBootPurpose,
        authenticated_handoff: Option<&HandoffBootPayload>,
    ) -> Result<()> {
        log::info!("[PE] 从ISO启动PE");

        crate::core::iso::IsoMounter::with_mounted_iso(iso_path, |mount_point| {
            log::info!("[PE] ISO已挂载到: {mount_point}");
            let wim_path = [
                format!("{}\\sources\\boot.wim", mount_point),
                format!("{}\\Boot\\boot.wim", mount_point),
                format!("{}\\boot.wim", mount_point),
                format!("{}\\BOOT\\BOOT.WIM", mount_point),
            ]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("ISO中未找到 boot.wim")))?;
            let sdi_path = [
                format!("{}\\boot\\boot.sdi", mount_point),
                format!("{}\\Boot\\boot.sdi", mount_point),
                format!("{}\\BOOT\\BOOT.SDI", mount_point),
            ]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("ISO中未找到有效的 boot.sdi")))?;

            copy_file_atomic(directory, Path::new(&wim_path), &record.wim_path)?;
            copy_file_atomic(directory, Path::new(&sdi_path), &record.sdi_path)?;
            Ok(())
        })?;

        if let Some(payload) = authenticated_handoff {
            inject_authenticated_handoff(directory, &record.wim_path, payload)
                .context("inject mandatory authenticated handoff into copied ISO boot WIM")?;
        }

        // 创建BCD引导项
        self.create_pe_boot_entry(display_name, record)?;

        // 7. 设置下次启动
        self.set_next_boot(&record.loader_guid)?;

        log::info!("[PE] ========== PE启动准备完成 ==========");
        Ok(())
    }

    /// 从WIM直接启动PE
    fn boot_from_wim(
        &self,
        directory: &SecurePeDirectory,
        wim_path: &str,
        display_name: &str,
        record: &PeBootRecord,
        purpose: PeBootPurpose,
        authenticated_handoff: Option<&HandoffBootPayload>,
    ) -> Result<()> {
        log::info!("[PE] 从WIM启动PE");

        // 1. 复制WIM到系统分区
        let target_wim = record.wim_path.to_string_lossy().into_owned();
        log::info!("[PE] 复制 WIM 到 {}", target_wim);
        copy_file_atomic(directory, Path::new(wim_path), &record.wim_path)?;

        // The private authoritative handoff is injected last. Recovery passwords may exist only
        // for the dedicated maintenance purpose and are manifest-bound inside this private WIM.
        if purpose.may_inject_bitlocker_recovery_material() {
            log::info!(
                "[PE] authenticated maintenance boot may carry a protected BitLocker secret"
            );
        }
        if let Some(payload) = authenticated_handoff {
            inject_authenticated_handoff(directory, Path::new(&target_wim), payload)
                .context("inject mandatory authenticated handoff into copied boot WIM")?;
        }

        // 2. 创建或使用boot.sdi
        let target_sdi = self.create_default_sdi(directory, &record.sdi_path)?;
        lr_core::scoped_temp_file::verify_system_administrators_file_custody(
            &open_regular_file_locked(Path::new(&target_sdi))?,
        )
        .context("verify persistent PE boot.sdi custody")?;

        // 3. 创建BCD引导项
        self.create_pe_boot_entry(display_name, record)?;

        // 4. 设置下次启动
        self.set_next_boot(&record.loader_guid)?;

        log::info!("[PE] ========== PE启动准备完成 ==========");
        Ok(())
    }

    /// 创建默认的boot.sdi文件
    fn create_default_sdi(&self, directory: &SecurePeDirectory, sdi_path: &Path) -> Result<String> {
        #[cfg(feature = "ci-automation")]
        if ci_missing_boot_sdi_fault_requested() {
            anyhow::bail!(
                "CI fault injection missing_boot_sdi: trusted boot.sdi copy was stopped before the PE boot entry was created"
            );
        }
        let system_directory = lr_core::windows_compat::system_directory()
            .context("resolve the running Windows System32 directory for boot.sdi")?;
        let windows_directory = system_directory
            .parent()
            .ok_or_else(|| anyhow::anyhow!("running Windows System32 has no Windows parent"))?;
        // 尝试从当前 Windows 系统目录复制，不假定系统卷盘符为 C。
        let system_sdi_paths = [
            windows_directory.join(r"Boot\DVD\PCAT\boot.sdi"),
            windows_directory.join(r"Boot\DVD\EFI\boot.sdi"),
        ];

        if let Some(source) = system_sdi_paths.iter().find(|path| path.is_file()) {
            log::info!("[PE] 从系统复制 boot.sdi: {}", source.display());
        }
        copy_first_boot_sdi(directory, sdi_path, &system_sdi_paths)
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn run_bcdedit(&self, arguments: &[&str]) -> Result<String> {
        let output = create_command(&self.bcdedit_path)
            .args(arguments)
            .output()
            .map_err(|error| {
                anyhow::anyhow!(
                    "{}",
                    tr!(
                        "无法启动 bcdedit（参数：{}）：{}",
                        arguments.join(" "),
                        error
                    )
                )
            })?;
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        ensure_bcdedit_success(
            arguments,
            output.status.success(),
            output.status.code(),
            &stdout,
            &stderr,
        )?;
        log::info!(
            "[PE] bcdedit {:?}: stdout={} stderr={}",
            arguments,
            stdout,
            stderr
        );
        Ok(stdout)
    }

    /// 创建PE引导项
    fn create_pe_boot_entry(&self, display_name: &str, record: &PeBootRecord) -> Result<()> {
        log::info!("[PE] 创建BCD引导项");

        let is_uefi = Self::is_uefi_boot()?;
        log::info!("[PE] 引导模式: {}", if is_uefi { "UEFI" } else { "Legacy" });

        // 转换路径为 BCD 格式，并将 ramdisk 设备绑定到实际运行中的 Windows 卷。
        let system_drive = lr_core::windows_storage::current_windows_drive_letter()
            .context("resolve running Windows volume for PE BCD entry")?;
        let wim_bcd_path = bcd_path_on_volume(&record.wim_path, system_drive)?;
        let sdi_bcd_path = bcd_path_on_volume(&record.sdi_path, system_drive)?;
        let system_partition = format!("partition={system_drive}:");

        let create_result = (|| -> Result<()> {
            log::info!("[PE] 创建 ramdisk 设备");
            let ram_description = format!("{} RAM", display_name);
            let ramdisk_guid = record.ramdisk_guid.as_str();
            self.run_bcdedit(&["/create", ramdisk_guid, "/d", &ram_description, "/device"])?;
            log::info!("[PE] Ramdisk GUID: {}", ramdisk_guid);

            for cmd in [
                vec![
                    "/set",
                    ramdisk_guid,
                    "ramdisksdidevice",
                    system_partition.as_str(),
                ],
                vec!["/set", ramdisk_guid, "ramdisksdipath", &sdi_bcd_path],
            ] {
                self.run_bcdedit(&cmd)?;
            }

            log::info!("[PE] 创建 osloader");
            let loader_guid = record.loader_guid.as_str();
            self.run_bcdedit(&[
                "/create",
                loader_guid,
                "/d",
                display_name,
                "/application",
                "osloader",
            ])?;
            log::info!("[PE] Loader GUID: {}", loader_guid);

            let winload = if is_uefi {
                "\\windows\\system32\\boot\\winload.efi"
            } else {
                "\\windows\\system32\\boot\\winload.exe"
            };
            let device_str = format!("ramdisk=[{system_drive}:]{},{}", wim_bcd_path, ramdisk_guid);
            for cmd in [
                vec!["/set", loader_guid, "device", &device_str],
                vec!["/set", loader_guid, "path", winload],
                vec!["/set", loader_guid, "osdevice", &device_str],
                vec!["/set", loader_guid, "systemroot", "\\windows"],
                vec!["/set", loader_guid, "detecthal", "yes"],
                vec!["/set", loader_guid, "winpe", "yes"],
                vec!["/set", loader_guid, "ems", "no"],
            ] {
                self.run_bcdedit(&cmd)?;
            }
            Ok(())
        })();

        create_result?;
        Ok(())
    }

    /// 设置下次启动为PE
    fn set_next_boot(&self, loader_guid: &str) -> Result<()> {
        log::info!("[PE] 设置下次启动: {}", loader_guid);
        add_one_shot_boot(self, loader_guid)
    }

    fn read_active_boot_records(&self, directory: &SecurePeDirectory) -> Result<Vec<PeBootRecord>> {
        let journal = active_pe_journal(directory);
        ensure_regular_or_absent(directory, &journal)?;
        match std::fs::read_to_string(&journal) {
            Ok(contents) => parse_boot_records(&directory.path, &contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn commit_boot_record(
        &self,
        directory: &SecurePeDirectory,
        record: &PeBootRecord,
    ) -> Result<()> {
        let records = self.read_active_boot_records(directory)?;
        if !records.iter().any(|existing| {
            existing
                .ramdisk_guid
                .eq_ignore_ascii_case(&record.ramdisk_guid)
                && existing
                    .loader_guid
                    .eq_ignore_ascii_case(&record.loader_guid)
        }) {
            let mut bytes = serialize_boot_records(&directory.path, &records)?;
            bytes.extend_from_slice(&serialize_boot_records(
                &directory.path,
                std::slice::from_ref(record),
            )?);
            return write_journal_bytes(directory, &active_pe_journal(directory), &bytes)
                .context("commit active PE BCD journal");
        }
        write_journal(directory, &active_pe_journal(directory), &records)
            .context("commit active PE BCD journal")
    }

    fn recover_pending_boot_transaction(&self, directory: &SecurePeDirectory) -> Result<()> {
        let journal = pending_pe_journal(directory);
        ensure_regular_or_absent(directory, &journal)?;
        let contents = match std::fs::read_to_string(&journal) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let records = parse_boot_records(&directory.path, &contents)
            .context("parse pending PE BCD journal")?;
        if records.len() != 1 {
            anyhow::bail!("pending PE BCD journal must contain exactly one record");
        }
        let record = &records[0];
        let committed = self
            .read_active_boot_records(directory)?
            .iter()
            .any(|existing| {
                existing
                    .ramdisk_guid
                    .eq_ignore_ascii_case(&record.ramdisk_guid)
                    && existing
                        .loader_guid
                        .eq_ignore_ascii_case(&record.loader_guid)
            });
        if committed {
            return remove_file_verified(directory, &journal);
        }
        self.rollback_boot_record(directory, record)
            .context("recover interrupted PE BCD transaction")?;
        remove_file_verified(directory, &journal)
    }

    fn rollback_boot_record(
        &self,
        directory: &SecurePeDirectory,
        record: &PeBootRecord,
    ) -> Result<()> {
        remove_one_shot_boot(self, &record.loader_guid)
            .context("unpublish PE loader from BootSequence")?;

        // Delete in exact reverse creation order. Do not remove a dependency or payload when the
        // object that references it could not be proven absent; the persistent journal makes the
        // remaining transaction safely retryable on the next run.
        self.delete_bcd_object_if_present(&record.loader_guid)
            .context("delete PE loader object")?;
        self.delete_bcd_object_if_present(&record.ramdisk_guid)
            .context("delete PE ramdisk object")?;

        let mut failures = Vec::new();
        for path in [&record.wim_path, &record.sdi_path] {
            if let Err(error) = remove_file_verified(directory, path) {
                failures.push(format!("remove PE payload {}: {error}", path.display()));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(failures.join("; "))
        }
    }

    fn delete_bcd_object_if_present(&self, guid: &str) -> Result<()> {
        delete_bcd_object_with_readback(self, guid)
    }

    fn cleanup_orphaned_private_pe_files(&self, directory: &SecurePeDirectory) -> Result<()> {
        let mut retained = Vec::new();
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&directory.path)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name_text) = name.to_str() else {
                retained.push(name.to_string_lossy().into_owned());
                continue;
            };
            if matches!(name_text, ACTIVE_PE_JOURNAL_NAME | PENDING_PE_JOURNAL_NAME) {
                // Journal ownership is handled only by the exact transaction recovery paths.
                retained.push(name_text.to_owned());
                continue;
            }
            if lr_core::handoff_auth::is_orphaned_private_pe_file_name(name_text) {
                candidates.push(entry.path());
            } else {
                retained.push(name_text.to_owned());
            }
        }
        for path in candidates {
            remove_file_verified(directory, &path)
                .with_context(|| format!("remove orphaned private PE file {}", path.display()))?;
            log::info!("[PE] 已清理上次任务遗留的私有 PE 文件: {}", path.display());
        }
        if !retained.is_empty() {
            retained.sort();
            retained.truncate(8);
            log::warn!(
                "[PE] 私有 PE 目录包含非任务命名的项目，已保留且不会阻止新任务: {}",
                retained.join(", ")
            );
        }
        Ok(())
    }

    /// 清理旧的PE引导项
    fn cleanup_old_pe_entries(&self, directory: &SecurePeDirectory) -> Result<()> {
        let journal = active_pe_journal(directory);
        ensure_regular_or_absent(directory, &journal)?;
        let content = match std::fs::read_to_string(&journal) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let records =
            parse_boot_records(&directory.path, &content).context("parse active PE BCD journal")?;
        for record in records.iter().rev() {
            self.rollback_boot_record(directory, record)?;
        }
        remove_file_verified(directory, &journal)
    }

    /// 重启系统
    pub fn reboot() {
        log::info!("[PE] 执行重启");
        if let Err(error) =
            lr_core::windows_shutdown::schedule_restart(3, "LetRecovery 正在重启到 PE 环境...")
        {
            log::error!("[PE] 安排重启失败: {error}");
        }
    }
}

fn validate_maintenance_language(language: &str) -> Result<&str> {
    if language.len() > 32
        || !language
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        anyhow::bail!("maintenance UI language is invalid");
    }
    Ok(language)
}

#[cfg(windows)]
fn build_maintenance_payload(
    language: &str,
    recovery_keys: &zeroize::Zeroizing<Vec<String>>,
) -> Result<HandoffBootPayload> {
    use lr_core::handoff_manifest::{
        ArtifactLocation, ArtifactRecord, ArtifactRole, HandoffManifest, ManifestBinding,
    };

    let language = validate_maintenance_language(language)?;
    let session_id = lr_core::handoff_auth::generate_session_id()?;
    let locator = lr_core::handoff_auth::generate_locator_token()?;
    let secret = if recovery_keys.is_empty() {
        None
    } else {
        Some(lr_core::bl_passthrough::serialize_keys(recovery_keys).map_err(anyhow::Error::msg)?)
    };
    let artifacts = secret
        .as_ref()
        .map(|bytes| {
            Ok::<ArtifactRecord, anyhow::Error>(ArtifactRecord {
                role: ArtifactRole::ProtectedBitLockerSecret,
                location: ArtifactLocation::ProtectedBoot,
                ordinal: 0,
                relative_path: lr_core::bl_passthrough::KEYS_FILE_NAME.to_owned(),
                length_bytes: bytes.len() as u64,
                sha256: lr_core::install_handoff::decode_hex_array::<32>(
                    &lr_core::hash::sha256_bytes(bytes),
                    "protected BitLocker secret SHA-256",
                )?,
            })
        })
        .transpose()?
        .into_iter()
        .collect();
    let manifest = HandoffManifest::new(
        lr_core::handoff_auth::HandoffPurpose::Maintenance,
        session_id.as_str(),
        locator.as_str(),
        None,
        None,
        artifacts,
    )?;
    let manifest_bytes = manifest.to_bytes()?;
    let binding = ManifestBinding::new(&manifest_bytes)?;
    let config_bytes = format!(
        "[Maintenance]\r\nSessionId={}\r\nLanguage={}\r\n{}",
        session_id.as_str(),
        language,
        binding.to_config_lines()
    )
    .into_bytes();
    let payload = HandoffBootPayload::new(
        lr_core::handoff_auth::SessionAuthKey::generate()?,
        lr_core::handoff_auth::HandoffPurpose::Maintenance,
        session_id.as_str(),
        config_bytes,
        manifest_bytes,
        None,
        None,
    )?;
    match secret {
        Some(secret) => payload.with_bitlocker_secret(secret),
        None => Ok(payload),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeMaintenanceProgress {
    LocatingPe,
    SnapshottingPe,
    CollectingBitLockerKeys,
    CreatingBootEntry,
    SchedulingRestart,
    RestartScheduled,
}

/// Prepare a one-shot authenticated maintenance boot, then schedule the restart. Recovery-password
/// collection is deliberately best-effort. An already-local WIM may be user-customized; catalogue
/// hashes are checked only by the download executor, never again at this launch boundary.
#[cfg(windows)]
pub(crate) fn enter_pe_maintenance(
    pe: &crate::download::config::OnlinePE,
    language: &str,
) -> Result<()> {
    enter_pe_maintenance_with_progress(pe, language, |_| {})
}

#[cfg(windows)]
pub(crate) fn enter_pe_maintenance_with_progress(
    pe: &crate::download::config::OnlinePE,
    language: &str,
    mut progress: impl FnMut(PeMaintenanceProgress),
) -> Result<()> {
    use lr_core::cached_artifact::CachedArtifactStatus;

    if crate::core::disk::DiskManager::is_pe_environment() {
        anyhow::bail!("PE maintenance entry is available only in normal Windows");
    }
    progress(PeMaintenanceProgress::LocatingPe);
    let path =
        match PeManager::check_cached_pe(&pe.filename, pe.sha256.as_deref(), pe.md5.as_deref())? {
            CachedArtifactStatus::Ready { path, .. } => path,
            CachedArtifactStatus::Missing => {
                anyhow::bail!("the selected PE has not been downloaded")
            }
        };
    progress(PeMaintenanceProgress::SnapshottingPe);
    let snapshot = snapshot_local_pe(&path, &pe.filename)?;
    progress(PeMaintenanceProgress::CollectingBitLockerKeys);
    let keys = zeroize::Zeroizing::new(
        crate::core::bitlocker::BitLockerManager::new().collect_recovery_keys_best_effort(),
    );
    let payload = build_maintenance_payload(language, &keys)?;
    progress(PeMaintenanceProgress::CreatingBootEntry);
    PeManager::new()
        .boot_to_pe_for_maintenance(&snapshot.path.to_string_lossy(), &pe.display_name, payload)?
        .commit()?;
    progress(PeMaintenanceProgress::SchedulingRestart);
    lr_core::windows_shutdown::schedule_restart(3, "LetRecovery 正在重启到 PE 维护环境...")
        .context("PE 维护环境已经准备完成，但 Windows 未能安排重启；下次手动重启仍会进入 PE")?;
    progress(PeMaintenanceProgress::RestartScheduled);
    Ok(())
}

fn inspect_pe_candidates(
    filename: &str,
    user_managed_directories: &[PathBuf],
    managed_cache_directories: &[PathBuf],
    _sha256: Option<&str>,
    _md5: Option<&str>,
) -> std::result::Result<CachedArtifactPresence, CachedArtifactError> {
    match inspect_cached_artifact(filename, user_managed_directories, None, None)? {
        present @ CachedArtifactPresence::Present { .. } => Ok(present),
        CachedArtifactPresence::Missing => {
            inspect_cached_artifact(filename, managed_cache_directories, None, None)
        }
    }
}

fn verify_pe_candidates(
    filename: &str,
    user_managed_directories: &[PathBuf],
    managed_cache_directories: &[PathBuf],
    _sha256: Option<&str>,
    _md5: Option<&str>,
) -> std::result::Result<CachedArtifactStatus, CachedArtifactError> {
    match verify_cached_artifact(filename, user_managed_directories, None, None)? {
        ready @ CachedArtifactStatus::Ready { .. } => Ok(ready),
        CachedArtifactStatus::Missing => {
            verify_cached_artifact(filename, managed_cache_directories, None, None)
        }
    }
}

impl Default for PeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod cache_policy_tests {
    use super::*;
    use lr_core::cached_artifact::CachedArtifactVerification;
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    const WRONG_MD5: &str = "00000000000000000000000000000000";
    const RAMDISK_GUID: &str = "{11111111-1111-1111-1111-111111111111}";
    const LOADER_GUID: &str = "{22222222-2222-2222-2222-222222222222}";
    const TEST_PERSISTENT_PE_DIR: &str = "C:\\LetRecovery_PE";

    #[cfg(feature = "ci-automation")]
    #[test]
    fn ci_boot_sdi_fault_requires_an_exact_session_shaped_value() {
        let run_id = "0123456789abcdef0123456789abcdef";
        assert!(ci_session_fault_value(
            &format!("missing_boot_sdi:{run_id}"),
            "missing_boot_sdi"
        ));
        assert!(ci_session_fault_value(
            &format!("after_auto_staging:{run_id}"),
            "after_auto_staging"
        ));
        assert!(ci_session_fault_value(
            &format!("auto_staging:{run_id}"),
            "auto_staging"
        ));
        assert!(!ci_session_fault_value(
            "missing_boot_sdi:short",
            "missing_boot_sdi"
        ));
        assert!(!ci_session_fault_value(
            "missing_boot_sdi:0123456789abcdef0123456789abcdeg",
            "missing_boot_sdi"
        ));
        assert!(!ci_session_fault_value(
            &format!("after_auto_staging:{run_id}"),
            "missing_boot_sdi"
        ));
    }

    #[test]
    fn only_maintenance_boot_policy_allows_recovery_material_injection() {
        assert!(!PeBootPurpose::Install.may_inject_bitlocker_recovery_material());
        assert!(!PeBootPurpose::Backup.may_inject_bitlocker_recovery_material());
        assert!(PeBootPurpose::Maintenance.may_inject_bitlocker_recovery_material());
    }

    #[test]
    fn install_payload_accepts_only_the_manifest_bound_administrator_secret() {
        use lr_core::handoff_manifest::{ArtifactLocation, ArtifactRecord, ArtifactRole};

        let session_id = "0123456789abcdef0123456789abcdef";
        let secret = lr_core::unattend_account::serialize_protected_administrator_secret(
            &lr_core::unattend_account::SensitiveString::new("temporary-secret"),
        )
        .unwrap();
        let sha256 = lr_core::install_handoff::decode_hex_array::<32>(
            &lr_core::hash::sha256_bytes(&secret),
            "test Administrator secret SHA-256",
        )
        .unwrap();
        let manifest = lr_core::handoff_manifest::HandoffManifest::new(
            lr_core::handoff_auth::HandoffPurpose::Install,
            session_id.to_owned(),
            "a".repeat(64),
            Some("b".repeat(64)),
            None,
            vec![
                ArtifactRecord {
                    role: ArtifactRole::InstallImageSpan,
                    location: ArtifactLocation::PublicData,
                    ordinal: 0,
                    relative_path: "LetRecovery_Data\\install.wim".to_owned(),
                    length_bytes: 1,
                    sha256: [0x11; 32],
                },
                ArtifactRecord {
                    role: ArtifactRole::ProtectedAdministratorSecret,
                    location: ArtifactLocation::ProtectedBoot,
                    ordinal: 0,
                    relative_path:
                        lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_FILE_NAME
                            .to_owned(),
                    length_bytes: secret.len() as u64,
                    sha256,
                },
            ],
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let binding = lr_core::handoff_manifest::ManifestBinding::new(&manifest).unwrap();
        let config = format!(
            "[Install]\r\nSessionId={session_id}\r\n{}",
            binding.to_config_lines()
        )
        .into_bytes();
        let payload = HandoffBootPayload::new(
            lr_core::handoff_auth::SessionAuthKey::from_bytes([0x5a; 32]).unwrap(),
            lr_core::handoff_auth::HandoffPurpose::Install,
            session_id,
            config.clone(),
            manifest.clone(),
            None,
            None,
        )
        .unwrap()
        .with_administrator_secret(secret)
        .unwrap();
        assert!(payload.administrator_secret.is_some());

        let wrong = lr_core::unattend_account::serialize_protected_administrator_secret(
            &lr_core::unattend_account::SensitiveString::new("different-secret"),
        )
        .unwrap();
        let payload = HandoffBootPayload::new(
            lr_core::handoff_auth::SessionAuthKey::from_bytes([0x5a; 32]).unwrap(),
            lr_core::handoff_auth::HandoffPurpose::Install,
            session_id,
            config,
            manifest,
            None,
            None,
        )
        .unwrap();
        assert!(payload.with_administrator_secret(wrong).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn maintenance_payload_without_recovery_keys_has_no_secret_or_disk_identity() {
        let payload =
            build_maintenance_payload("zh-CN", &zeroize::Zeroizing::new(Vec::new())).unwrap();
        let manifest =
            lr_core::handoff_manifest::HandoffManifest::parse(&payload.manifest_bytes).unwrap();

        assert_eq!(
            manifest.purpose,
            lr_core::handoff_auth::HandoffPurpose::Maintenance
        );
        assert!(manifest.install_target_token.is_none());
        assert!(manifest.auto_staging.is_none());
        assert!(manifest.artifacts.is_empty());
        assert!(payload.bitlocker_secret.is_none());
        assert!(std::str::from_utf8(&payload.config_bytes)
            .unwrap()
            .contains("[Maintenance]\r\n"));
    }

    #[cfg(windows)]
    #[test]
    fn maintenance_payload_binds_only_the_protected_recovery_password_bundle() {
        use lr_core::handoff_manifest::{ArtifactLocation, ArtifactRole};

        let key = "111111-222222-333333-444444-555555-666666-777777-888888".to_owned();
        let payload =
            build_maintenance_payload("en-US", &zeroize::Zeroizing::new(vec![key.clone()]))
                .unwrap();
        let manifest =
            lr_core::handoff_manifest::HandoffManifest::parse(&payload.manifest_bytes).unwrap();
        let artifact = manifest.artifacts.first().unwrap();
        let secret = payload.bitlocker_secret.as_ref().unwrap();

        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(artifact.role, ArtifactRole::ProtectedBitLockerSecret);
        assert_eq!(artifact.location, ArtifactLocation::ProtectedBoot);
        assert_eq!(
            artifact.relative_path,
            lr_core::bl_passthrough::KEYS_FILE_NAME
        );
        assert_eq!(artifact.length_bytes, secret.len() as u64);
        assert_eq!(
            lr_core::bl_passthrough::parse_keys(secret)
                .unwrap()
                .as_slice(),
            &[key]
        );
        assert!(manifest.install_target_token.is_none());
        assert!(manifest.auto_staging.is_none());
    }

    #[test]
    fn persistent_pe_root_acl_accepts_only_protected_admin_system_entries() {
        assert!(trusted_pe_directory_sddl(
            "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        ));
        assert!(trusted_pe_directory_sddl(
            "O:SYG:SYD:PAI(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)"
        ));
        assert!(!trusted_pe_directory_sddl(
            "O:BAG:BAD:(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        ));
        assert!(!trusted_pe_directory_sddl(
            "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;BU)"
        ));
    }

    #[test]
    fn private_pe_paths_follow_a_non_c_windows_volume() {
        let root = Path::new(r"D:\LetRecovery_PE");
        let record = PeBootRecord {
            ramdisk_guid: RAMDISK_GUID.to_owned(),
            loader_guid: LOADER_GUID.to_owned(),
            wim_path: root.join("boot-0123456789abcdef0123456789abcdef.wim"),
            sdi_path: root.join("boot-0123456789abcdef0123456789abcdef.sdi"),
            session_id: None,
            root_identity: None,
            handoff_purpose: None,
            handoff_capsule_sha256: None,
        };
        let serialized = serialize_boot_records(root, std::slice::from_ref(&record)).unwrap();
        let parsed = parse_boot_records(root, std::str::from_utf8(&serialized).unwrap()).unwrap();
        assert_eq!(parsed[0].wim_path, record.wim_path);
        assert_eq!(
            bcd_path_on_volume(&record.wim_path, 'D').unwrap(),
            r"\LetRecovery_PE\boot-0123456789abcdef0123456789abcdef.wim"
        );
        assert!(bcd_path_on_volume(&record.wim_path, 'C').is_err());

        let legacy =
            parse_boot_records(root, &format!("{RAMDISK_GUID}\r\n{LOADER_GUID}\r\n")).unwrap();
        assert_eq!(legacy[0].wim_path, root.join("boot.wim"));
    }

    #[test]
    fn stale_private_pe_cleanup_recognizes_only_bounded_product_names() {
        for name in [
            "boot.wim",
            "boot.sdi",
            "boot-0123456789abcdef0123456789abcdef.wim",
            "boot-01234567-89ab-cdef-0123-456789abcdef.sdi",
            "pe-payload-123-4.tmp",
            "pe-bcd-journal-123-4.tmp",
            "handoff-capsule-123-4.txt",
            "handoff-config-123-4.ini",
            "handoff-manifest-123-4.txt",
            "handoff-unattend-123-4.xml",
            "handoff-wifi-123-4.xml",
        ] {
            assert!(
                lr_core::handoff_auth::is_orphaned_private_pe_file_name(name),
                "{name}"
            );
        }
        for name in [
            "pe_guid.txt",
            "pe_pending.txt",
            "boot-user-notes.txt",
            "boot-..\\Windows.wim",
            "handoff-config-user.ini.bak",
            "handoff-config-.ini",
            "handoff-config-deadbeef.ini",
            "handoff-config-123.ini",
            "handoff-config-123-4-5.ini",
            "photos",
        ] {
            assert!(
                !lr_core::handoff_auth::is_orphaned_private_pe_file_name(name),
                "{name}"
            );
        }
    }

    enum MockReply {
        Ok(String),
        Err(&'static str),
    }

    struct MockBcdRunner {
        replies: Mutex<VecDeque<MockReply>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl MockBcdRunner {
        fn new(replies: impl IntoIterator<Item = MockReply>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl BcdRunner for MockBcdRunner {
        fn run(&self, arguments: &[&str]) -> Result<String> {
            self.calls
                .lock()
                .expect("lock mock BCD calls")
                .push(arguments.iter().map(|value| value.to_string()).collect());
            match self
                .replies
                .lock()
                .expect("lock mock BCD replies")
                .pop_front()
                .expect("mock BCD reply")
            {
                MockReply::Ok(output) => Ok(output),
                MockReply::Err(message) => anyhow::bail!(message),
            }
        }
    }

    #[test]
    fn bcdedit_nonzero_exit_is_never_treated_as_success() {
        let error = ensure_bcdedit_success(
            &["/bootsequence", "{fixture}"],
            false,
            Some(5),
            "",
            "access denied",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("/bootsequence"));
        assert!(error.contains('5'));
        assert!(error.contains("access denied"));
    }

    #[test]
    fn bcdedit_zero_exit_is_accepted() {
        ensure_bcdedit_success(&["/enum", "{bootmgr}"], true, Some(0), "ok", "").unwrap();
    }

    #[test]
    fn one_shot_boot_uses_addfirst_and_requires_readback() {
        let runner = MockBcdRunner::new([
            MockReply::Ok(String::new()),
            MockReply::Ok(format!("bootsequence {LOADER_GUID}")),
        ]);

        add_one_shot_boot(&runner, LOADER_GUID).unwrap();

        assert_eq!(
            *runner.calls.lock().unwrap(),
            vec![
                vec!["/bootsequence", LOADER_GUID, "/addfirst"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                vec!["/enum", "{bootmgr}"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            ]
        );
    }

    #[test]
    fn one_shot_boot_fails_when_bootmanager_readback_omits_loader() {
        let runner = MockBcdRunner::new([
            MockReply::Ok(String::new()),
            MockReply::Ok("bootsequence {33333333-3333-3333-3333-333333333333}".to_string()),
        ]);

        let error = add_one_shot_boot(&runner, LOADER_GUID)
            .expect_err("missing loader readback must fail")
            .to_string();

        assert!(error.contains("absent from BootSequence"));
    }

    #[test]
    fn rollback_accepts_remove_error_only_when_readback_proves_absence() {
        let runner = MockBcdRunner::new([
            MockReply::Err("object was already absent"),
            MockReply::Ok("bootsequence {33333333-3333-3333-3333-333333333333}".to_string()),
        ]);
        remove_one_shot_boot(&runner, LOADER_GUID).unwrap();

        let unsafe_runner = MockBcdRunner::new([
            MockReply::Err("access denied"),
            MockReply::Ok(format!("bootsequence {LOADER_GUID}")),
        ]);
        let error = remove_one_shot_boot(&unsafe_runner, LOADER_GUID)
            .expect_err("loader still present must fail")
            .to_string();
        assert!(error.contains("access denied"));
    }

    #[test]
    fn bcd_object_lookup_accepts_zero_exit_without_requested_guid_as_absent() {
        let runner = MockBcdRunner::new([
            MockReply::Ok("没有匹配的对象或存储为空。".to_string()),
            MockReply::Ok("identifier              {bootmgr}".to_string()),
        ]);

        assert!(!bcd_object_is_present(&runner, LOADER_GUID).unwrap());
        assert_eq!(
            *runner.calls.lock().unwrap(),
            vec![
                vec!["/enum", LOADER_GUID, "/v"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                vec!["/enum", "{bootmgr}", "/v"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            ]
        );
    }

    #[test]
    fn bcd_object_lookup_does_not_confuse_another_guid_with_requested_object() {
        let runner = MockBcdRunner::new([
            MockReply::Ok(
                "identifier              {33333333-3333-3333-3333-333333333333}".to_string(),
            ),
            MockReply::Ok("identifier              {bootmgr}".to_string()),
        ]);

        assert!(!bcd_object_is_present(&runner, LOADER_GUID).unwrap());
    }

    #[test]
    fn bcd_object_delete_accepts_zero_exit_empty_enumeration_after_delete() {
        let runner = MockBcdRunner::new([
            MockReply::Ok(format!("identifier              {LOADER_GUID}")),
            MockReply::Ok("操作成功完成。".to_string()),
            MockReply::Ok("没有匹配的对象或存储为空。".to_string()),
            MockReply::Ok("identifier              {bootmgr}".to_string()),
        ]);

        delete_bcd_object_with_readback(&runner, LOADER_GUID).unwrap();
        assert_eq!(
            *runner.calls.lock().unwrap(),
            vec![
                vec!["/enum", LOADER_GUID, "/v"],
                vec!["/delete", LOADER_GUID],
                vec!["/enum", LOADER_GUID, "/v"],
                vec!["/enum", "{bootmgr}", "/v"],
            ]
            .into_iter()
            .map(|arguments| arguments
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn bcd_object_absence_is_rejected_when_the_store_cannot_be_read() {
        let runner = MockBcdRunner::new([
            MockReply::Err("object query failed"),
            MockReply::Err("BCD store unreadable"),
        ]);

        let error = bcd_object_is_present(&runner, LOADER_GUID)
            .expect_err("an unreadable store must not be interpreted as absence")
            .to_string();
        assert!(error.contains("cannot distinguish"));
        assert!(error.contains("object query failed"));
    }

    #[test]
    fn bcd_object_delete_fails_when_readback_still_lists_the_guid() {
        let runner = MockBcdRunner::new([
            MockReply::Ok(format!("identifier              {LOADER_GUID}")),
            MockReply::Ok("操作成功完成。".to_string()),
            MockReply::Ok(format!("identifier              {LOADER_GUID}")),
        ]);

        let error = delete_bcd_object_with_readback(&runner, LOADER_GUID)
            .expect_err("a GUID still present after delete must fail")
            .to_string();
        assert!(error.contains("remains"));
    }

    #[test]
    fn versioned_boot_journal_roundtrips_multiple_session_payloads() {
        let records = vec![
            PeBootRecord {
                ramdisk_guid: RAMDISK_GUID.to_string(),
                loader_guid: LOADER_GUID.to_string(),
                wim_path: PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot-first.wim")),
                sdi_path: PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot-first.sdi")),
                session_id: None,
                root_identity: None,
                handoff_purpose: None,
                handoff_capsule_sha256: None,
            },
            PeBootRecord {
                ramdisk_guid: "{33333333-3333-3333-3333-333333333333}".to_string(),
                loader_guid: "{44444444-4444-4444-4444-444444444444}".to_string(),
                wim_path: PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot-second.wim")),
                sdi_path: PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot-second.sdi")),
                session_id: None,
                root_identity: None,
                handoff_purpose: None,
                handoff_capsule_sha256: None,
            },
        ];

        let root = Path::new(TEST_PERSISTENT_PE_DIR);
        let serialized = serialize_boot_records(root, &records).unwrap();
        let parsed = parse_boot_records(root, std::str::from_utf8(&serialized).unwrap()).unwrap();

        assert_eq!(parsed.len(), records.len());
        for (actual, expected) in parsed.iter().zip(&records) {
            assert_eq!(actual.ramdisk_guid, expected.ramdisk_guid);
            assert_eq!(actual.loader_guid, expected.loader_guid);
            assert_eq!(actual.wim_path, expected.wim_path);
            assert_eq!(actual.sdi_path, expected.sdi_path);
        }
    }

    #[test]
    fn lrpe4_journal_roundtrips_authenticated_session_and_stable_root_identity() {
        let record = PeBootRecord {
            ramdisk_guid: RAMDISK_GUID.to_string(),
            loader_guid: LOADER_GUID.to_string(),
            wim_path: PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot-session.wim")),
            sdi_path: PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot-session.sdi")),
            session_id: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            root_identity: Some(lr_core::install_handoff::CanonicalInstallTargetV2 {
                layout_digest: [0x11; 32],
                device_id_hash: Some([0x22; 32]),
                partition_offset_bytes: 1_048_576,
                partition_length_bytes: 8_000_000,
                style: lr_core::install_handoff::CanonicalTargetStyle::Gpt,
                gpt_partition_id: Some([0x33; 16]),
            }),
            handoff_purpose: Some(lr_core::handoff_auth::HandoffPurpose::Install),
            handoff_capsule_sha256: Some([0x44; 32]),
        };
        let root = Path::new(TEST_PERSISTENT_PE_DIR);
        let serialized = serialize_boot_records(root, std::slice::from_ref(&record)).unwrap();
        assert!(std::str::from_utf8(&serialized)
            .unwrap()
            .starts_with("LRPE4\t"));
        let parsed = parse_boot_records(root, std::str::from_utf8(&serialized).unwrap()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].session_id, record.session_id);
        assert_eq!(parsed[0].root_identity, record.root_identity);
        assert_eq!(parsed[0].handoff_purpose, record.handoff_purpose);
        assert_eq!(
            parsed[0].handoff_capsule_sha256,
            record.handoff_capsule_sha256
        );
    }

    #[test]
    fn legacy_two_guid_journal_remains_readable() {
        let parsed = parse_boot_records(
            Path::new(TEST_PERSISTENT_PE_DIR),
            &format!("{RAMDISK_GUID}\r\n{LOADER_GUID}\r\n"),
        )
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].ramdisk_guid, RAMDISK_GUID);
        assert_eq!(parsed[0].loader_guid, LOADER_GUID);
        assert_eq!(
            parsed[0].wim_path,
            PathBuf::from(format!("{TEST_PERSISTENT_PE_DIR}\\boot.wim"))
        );
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "letrecovery-pe-policy-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create isolated test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn secure_test_directory(fixture: &TestDirectory) -> SecurePeDirectory {
        SecurePeDirectory {
            path: fixture.0.clone(),
            _lock: open_directory_without_delete_share(&fixture.0)
                .expect("lock isolated test directory"),
        }
    }

    #[cfg(windows)]
    fn assert_non_elevated_secure_owner_failure(error: &anyhow::Error, target: &Path) {
        const ERROR_INVALID_OWNER: i32 = 1307;
        const HRESULT_FROM_WIN32_ERROR_INVALID_OWNER: i32 = 0x8007_051B_u32 as i32;

        assert!(
            !crate::utils::privilege::is_admin(),
            "an elevated secure-copy test must not accept an owner-assignment failure: {error:#}"
        );
        assert!(
            error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error)
                    .is_some_and(|code| {
                        code == ERROR_INVALID_OWNER
                            || code == HRESULT_FROM_WIN32_ERROR_INVALID_OWNER
                    })
            }),
            "non-elevated secure-copy test failed for an unexpected reason: {error:#}"
        );
        assert!(
            !target.exists(),
            "owner-assignment failure must not publish the destination"
        );
    }

    #[test]
    fn observed_pe_copy_binds_locked_source_bytes() {
        let fixture = TestDirectory::new("authorized-snapshot");
        let source = fixture.0.join("source.wim");
        let target = fixture.0.join("snapshot.wim");
        let payload = b"locally managed PE payload";
        fs::write(&source, payload).unwrap();
        let sha256 = lr_core::hash::sha256_bytes(payload);
        let directory = secure_test_directory(&fixture);
        let (observed_size, observed_sha256) =
            match copy_file_atomic_observed(&directory, &source, &target) {
                Ok(observed) => observed,
                #[cfg(windows)]
                Err(error) => {
                    assert_non_elevated_secure_owner_failure(&error, &target);
                    return;
                }
                #[cfg(not(windows))]
                Err(error) => panic!("secure PE copy failed: {error:#}"),
            };
        assert_eq!(observed_size, payload.len() as u64);
        assert_eq!(observed_sha256, sha256);
        assert_eq!(fs::read(target).unwrap(), payload);
    }

    #[cfg(windows)]
    #[test]
    fn locked_snapshot_file_denies_write_rename_and_delete_until_consumed() {
        let fixture = TestDirectory::new("snapshot-lock");
        let path = fixture.0.join("snapshot.wim");
        let renamed = fixture.0.join("replaced.wim");
        let payload = b"locked snapshot payload";
        fs::write(&path, payload).unwrap();
        let mut held = open_regular_file_locked(&path).unwrap();
        assert!(fs::write(&path, b"replacement").is_err());
        assert!(fs::rename(&path, &renamed).is_err());
        assert!(fs::remove_file(&path).is_err());
        assert_eq!(
            lr_core::hash::sha256_reader(&mut held, |_| {}).unwrap(),
            lr_core::hash::sha256_bytes(payload)
        );
        drop(held);
        fs::rename(&path, &renamed).unwrap();
    }

    #[test]
    fn missing_boot_sdi_fails_without_creating_placeholder() {
        let fixture = TestDirectory::new("missing-sdi");
        let directory = secure_test_directory(&fixture);
        let target = fixture.0.join("boot.sdi");

        let error =
            copy_first_boot_sdi(&directory, &target, &[fixture.0.join("missing-source.sdi")])
                .unwrap_err()
                .to_string();

        assert!(error.contains("boot.sdi"));
        assert!(!target.exists());
    }

    #[test]
    fn boot_sdi_copy_requires_and_preserves_a_real_source() {
        let fixture = TestDirectory::new("copy-sdi");
        let directory = secure_test_directory(&fixture);
        let source = fixture.0.join("source.sdi");
        let target = fixture.0.join("target.sdi");
        let bytes = b"trusted boot sdi fixture";
        fs::write(&source, bytes).unwrap();

        let copied = match copy_first_boot_sdi(&directory, &target, std::slice::from_ref(&source)) {
            Ok(copied) => copied,
            #[cfg(windows)]
            Err(error) => {
                assert_non_elevated_secure_owner_failure(&error, &target);
                return;
            }
            #[cfg(not(windows))]
            Err(error) => panic!("secure boot.sdi copy failed: {error:#}"),
        };

        assert_eq!(copied, target);
        assert_eq!(fs::read(copied).unwrap(), bytes);
    }

    #[test]
    fn user_managed_pe_can_be_customized_without_matching_server_hash() {
        let local = TestDirectory::new("local");
        let managed = TestDirectory::new("managed-empty");
        let path = local.0.join("LetRecovery_PE.wim");
        fs::write(&path, b"custom PE contents").unwrap();

        let status = verify_pe_candidates(
            "LetRecovery_PE.wim",
            std::slice::from_ref(&local.0),
            std::slice::from_ref(&managed.0),
            None,
            Some(WRONG_MD5),
        )
        .unwrap();

        assert_eq!(
            status,
            CachedArtifactStatus::Ready {
                path,
                verification: CachedArtifactVerification::NotProvided,
            }
        );
    }

    #[test]
    fn customized_managed_download_cache_is_accepted_after_download() {
        let local = TestDirectory::new("local-empty");
        let managed = TestDirectory::new("managed");
        let path = managed.0.join("LetRecovery_PE.wim");
        fs::write(&path, b"user-customized PE after download").unwrap();

        let status = verify_pe_candidates(
            "LetRecovery_PE.wim",
            std::slice::from_ref(&local.0),
            std::slice::from_ref(&managed.0),
            None,
            Some(WRONG_MD5),
        )
        .unwrap();

        assert_eq!(
            status,
            CachedArtifactStatus::Ready {
                path,
                verification: CachedArtifactVerification::NotProvided,
            }
        );
    }
}
