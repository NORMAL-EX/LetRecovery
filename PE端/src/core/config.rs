use anyhow::{anyhow, bail, Context, Result};
use lr_core::boot_pca::BootPcaMode;
use lr_core::unattend_account::BuiltInAdministratorOptions;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const HANDOFF_CAPSULE_PATH: &str = r"X:\LR_HandoffAuth.txt";
const HANDOFF_CONFIG_PATH: &str = r"X:\LR_HandoffConfig.ini";
const HANDOFF_MANIFEST_PATH: &str = r"X:\LR_HandoffManifest.txt";
const HANDOFF_WIFI_PATH: &str = r"X:\LR_WifiProfile.xml";
const HANDOFF_ADMINISTRATOR_PATH: &str = r"X:\LR_AdministratorSecret.txt";
const HANDOFF_BITLOCKER_PATH: &str = r"X:\LR_BitLockerKeys.txt";

struct LockedBootPayloadFile {
    path: PathBuf,
    _file: File,
    bytes: Zeroizing<Vec<u8>>,
    _ancestor_pins: lr_core::scoped_temp_file::PinnedDirectoryAncestors,
}

impl LockedBootPayloadFile {
    fn open(path: &Path, maximum_bytes: u64) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("boot payload path has no parent"))?;
        let pins = lr_core::scoped_temp_file::pin_existing_directory_ancestors(parent)
            .with_context(|| format!("pin boot payload ancestors for {}", path.display()))?;
        pins.verify_unchanged()?;

        #[cfg(windows)]
        let mut file = {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            use windows::Win32::Foundation::GENERIC_READ;
            use windows::Win32::Storage::FileSystem::{
                DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            };
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                // DELETE is requested up front so marker removal later acts on this same held
                // object. FILE_SHARE_READ alone deliberately denies concurrent write/delete;
                // there is no path reopen between token comparison and task consumption.
                .access_mode(GENERIC_READ.0 | DELETE.0)
                .share_mode(FILE_SHARE_READ.0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
            let file = options
                .open(path)
                .with_context(|| format!("open fixed boot payload {}", path.display()))?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            {
                bail!(
                    "fixed boot payload is not an ordinary file: {}",
                    path.display()
                );
            }
            file
        };
        #[cfg(not(windows))]
        let mut file = {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!(
                    "fixed boot payload is not an ordinary file: {}",
                    path.display()
                );
            }
            File::open(path)?
        };

        let declared = file.metadata()?.len();
        if declared == 0 || declared > maximum_bytes {
            bail!(
                "fixed boot payload size {} is outside 1..={} bytes: {}",
                declared,
                maximum_bytes,
                path.display()
            );
        }
        let capacity = usize::try_from(declared).context("boot payload length exceeds usize")?;
        let mut bytes = Vec::with_capacity(capacity);
        file.by_ref()
            .take(maximum_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != declared || bytes.len() as u64 > maximum_bytes {
            bail!(
                "fixed boot payload changed length while being read: {}",
                path.display()
            );
        }
        pins.verify_unchanged()?;
        Ok(Self {
            path: path.to_path_buf(),
            _file: file,
            bytes: Zeroizing::new(bytes),
            _ancestor_pins: pins,
        })
    }

    fn delete_same_locked_file(self) -> Result<()> {
        self._ancestor_pins.verify_unchanged()?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::{BOOLEAN, HANDLE};
            use windows::Win32::Storage::FileSystem::{
                FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
            };
            let disposition = FILE_DISPOSITION_INFO {
                DeleteFile: BOOLEAN(1),
            };
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(self._file.as_raw_handle()),
                    FileDispositionInfo,
                    std::ptr::addr_of!(disposition).cast(),
                    u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                        .expect("FILE_DISPOSITION_INFO size fits u32"),
                )
            }
            .with_context(|| {
                format!(
                    "mark authenticated handoff file delete-on-close: {}",
                    self.path.display()
                )
            })?;
            let path = self.path.clone();
            drop(self);
            verify_path_absent(&path)
        }
        #[cfg(not(windows))]
        {
            let path = self.path.clone();
            drop(self);
            remove_file_and_verify_absent(&path)
        }
    }
}

/// Move-only proof that the running private boot WIM authorized one exact operation/config pair.
/// It owns deny-write/delete file handles for both fixed X: payloads, so later code cannot
/// accidentally authenticate one object and parse another by reopening a public path.
pub struct AuthenticatedOperationGuard {
    capsule: lr_core::handoff_auth::HandoffAuthCapsule,
    capsule_file: LockedBootPayloadFile,
    config_file: LockedBootPayloadFile,
    manifest_file: LockedBootPayloadFile,
    manifest: lr_core::handoff_manifest::HandoffManifest,
    private_wifi_profile: Option<LockedBootPayloadFile>,
    administrator_secret: Option<LockedBootPayloadFile>,
    bitlocker_secret: Option<LockedBootPayloadFile>,
}

impl AuthenticatedOperationGuard {
    pub fn discover() -> Result<Self> {
        let capsule_file = LockedBootPayloadFile::open(
            Path::new(HANDOFF_CAPSULE_PATH),
            lr_core::handoff_auth::AUTH_CAPSULE_MAX_BYTES as u64,
        )?;
        let config_file = LockedBootPayloadFile::open(
            Path::new(HANDOFF_CONFIG_PATH),
            lr_core::handoff_auth::AUTH_CONFIG_MAX_BYTES as u64,
        )?;
        let manifest_file = LockedBootPayloadFile::open(
            Path::new(HANDOFF_MANIFEST_PATH),
            lr_core::handoff_manifest::HANDOFF_MANIFEST_MAX_BYTES as u64,
        )?;
        let mut guard = Self::authenticate(capsule_file, config_file, manifest_file)?;
        let config_text = std::str::from_utf8(&guard.config_file.bytes)
            .context("authenticated handoff config is not UTF-8")?;
        if let Some(binding) =
            lr_core::first_logon::private_wifi_binding_from_install_ini(config_text)?
        {
            let wifi = LockedBootPayloadFile::open(
                Path::new(HANDOFF_WIFI_PATH),
                lr_core::first_logon::PRIVATE_WIFI_PROFILE_MAX_BYTES,
            )?;
            binding.verify(&wifi.bytes)?;
            guard.private_wifi_profile = Some(wifi);
        }
        let administrator_records = guard
            .manifest
            .artifacts
            .iter()
            .filter(|record| {
                record.role == lr_core::handoff_manifest::ArtifactRole::ProtectedAdministratorSecret
            })
            .collect::<Vec<_>>();
        if let [record] = administrator_records.as_slice() {
            if guard.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Install
                || record.location != lr_core::handoff_manifest::ArtifactLocation::ProtectedBoot
                || record.relative_path
                    != lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_FILE_NAME
            {
                bail!("protected Administrator secret has an invalid install binding");
            }
            let secret = LockedBootPayloadFile::open(
                Path::new(HANDOFF_ADMINISTRATOR_PATH),
                lr_core::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_MAX_BYTES,
            )?;
            let actual_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
                &lr_core::hash::sha256_bytes(&secret.bytes),
                "protected Administrator secret SHA-256",
            )?;
            if record.length_bytes != secret.bytes.len() as u64 || record.sha256 != actual_sha256 {
                bail!("protected Administrator secret does not match its manifest binding");
            }
            lr_core::unattend_account::parse_protected_administrator_secret(&secret.bytes)
                .map_err(anyhow::Error::msg)?;
            guard.administrator_secret = Some(secret);
        } else if !administrator_records.is_empty() {
            bail!("install handoff has more than one protected Administrator secret");
        }
        let bitlocker_records = guard
            .manifest
            .artifacts
            .iter()
            .filter(|record| {
                record.role == lr_core::handoff_manifest::ArtifactRole::ProtectedBitLockerSecret
            })
            .collect::<Vec<_>>();
        if let [record] = bitlocker_records.as_slice() {
            if guard.capsule.purpose() != lr_core::handoff_auth::HandoffPurpose::Maintenance
                || record.location != lr_core::handoff_manifest::ArtifactLocation::ProtectedBoot
                || record.relative_path != lr_core::bl_passthrough::KEYS_FILE_NAME
            {
                bail!("protected BitLocker secret has an invalid maintenance binding");
            }
            let secret = LockedBootPayloadFile::open(
                Path::new(HANDOFF_BITLOCKER_PATH),
                lr_core::bl_passthrough::MAX_BUNDLE_BYTES,
            )?;
            let actual_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
                &lr_core::hash::sha256_bytes(&secret.bytes),
                "protected BitLocker secret SHA-256",
            )?;
            if record.length_bytes != secret.bytes.len() as u64 || record.sha256 != actual_sha256 {
                bail!("protected BitLocker secret does not match its manifest binding");
            }
            lr_core::bl_passthrough::parse_keys(&secret.bytes).map_err(anyhow::Error::msg)?;
            guard.bitlocker_secret = Some(secret);
        } else if !bitlocker_records.is_empty() {
            bail!("maintenance handoff has more than one protected BitLocker secret");
        }
        Ok(guard)
    }

    fn authenticate(
        capsule_file: LockedBootPayloadFile,
        config_file: LockedBootPayloadFile,
        manifest_file: LockedBootPayloadFile,
    ) -> Result<Self> {
        let capsule = lr_core::handoff_auth::HandoffAuthCapsule::parse(&capsule_file.bytes)?;
        capsule.verify_config(capsule.purpose(), &config_file.bytes)?;
        let manifest_binding =
            lr_core::handoff_manifest::ManifestBinding::from_config_bytes(&config_file.bytes)?;
        let manifest = manifest_binding.verify(&manifest_file.bytes)?;
        if manifest.purpose != capsule.purpose() || manifest.session_id != capsule.session_id() {
            bail!("handoff manifest purpose/session does not match authentication capsule");
        }
        Ok(Self {
            capsule,
            capsule_file,
            config_file,
            manifest_file,
            manifest,
            private_wifi_profile: None,
            administrator_secret: None,
            bitlocker_secret: None,
        })
    }

    pub fn operation_type(&self) -> Option<OperationType> {
        match self.capsule.purpose() {
            lr_core::handoff_auth::HandoffPurpose::Install => Some(OperationType::Install),
            lr_core::handoff_auth::HandoffPurpose::Backup => Some(OperationType::Backup),
            lr_core::handoff_auth::HandoffPurpose::Expand => Some(OperationType::Expand),
            lr_core::handoff_auth::HandoffPurpose::Maintenance => None,
        }
    }

    pub fn session_id(&self) -> &str {
        self.capsule.session_id()
    }

    pub fn purpose(&self) -> lr_core::handoff_auth::HandoffPurpose {
        self.capsule.purpose()
    }

    pub fn capsule_sha256(&self) -> Result<[u8; 32]> {
        lr_core::install_handoff::decode_hex_array::<32>(
            &lr_core::hash::sha256_bytes(&self.capsule_file.bytes),
            "running handoff capsule SHA-256",
        )
    }

    pub fn exact_config_bytes(&self) -> &[u8] {
        &self.config_file.bytes
    }

    pub fn manifest(&self) -> &lr_core::handoff_manifest::HandoffManifest {
        &self.manifest
    }

    pub fn protected_bitlocker_secret_bytes(&self) -> Option<&[u8]> {
        self.bitlocker_secret
            .as_ref()
            .map(|secret| secret.bytes.as_slice())
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        self.capsule_file._ancestor_pins.verify_unchanged()?;
        self.config_file._ancestor_pins.verify_unchanged()?;
        self.manifest_file._ancestor_pins.verify_unchanged()?;
        if let Some(wifi) = &self.private_wifi_profile {
            wifi._ancestor_pins.verify_unchanged()?;
            let content = std::str::from_utf8(&self.config_file.bytes)
                .context("authenticated handoff config is not UTF-8")?;
            let binding = lr_core::first_logon::private_wifi_binding_from_install_ini(content)?
                .context("private Wi-Fi file exists without an authenticated binding")?;
            binding.verify(&wifi.bytes)?;
        }
        if let Some(secret) = &self.administrator_secret {
            secret._ancestor_pins.verify_unchanged()?;
            let record = self
                .manifest
                .artifacts
                .iter()
                .find(|record| {
                    record.role
                        == lr_core::handoff_manifest::ArtifactRole::ProtectedAdministratorSecret
                })
                .context("protected Administrator secret exists without a manifest binding")?;
            let actual_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
                &lr_core::hash::sha256_bytes(&secret.bytes),
                "protected Administrator secret SHA-256",
            )?;
            if record.length_bytes != secret.bytes.len() as u64 || record.sha256 != actual_sha256 {
                bail!("protected Administrator secret changed after authentication");
            }
            lr_core::unattend_account::parse_protected_administrator_secret(&secret.bytes)
                .map_err(anyhow::Error::msg)?;
        }
        if let Some(secret) = &self.bitlocker_secret {
            secret._ancestor_pins.verify_unchanged()?;
            let record = self
                .manifest
                .artifacts
                .iter()
                .find(|record| {
                    record.role == lr_core::handoff_manifest::ArtifactRole::ProtectedBitLockerSecret
                })
                .context("protected BitLocker secret exists without a manifest binding")?;
            let actual_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
                &lr_core::hash::sha256_bytes(&secret.bytes),
                "protected BitLocker secret SHA-256",
            )?;
            if record.length_bytes != secret.bytes.len() as u64 || record.sha256 != actual_sha256 {
                bail!("protected BitLocker secret changed after authentication");
            }
        }
        self.capsule
            .verify_config(self.capsule.purpose(), &self.config_file.bytes)?;
        lr_core::handoff_manifest::ManifestBinding::from_config_bytes(&self.config_file.bytes)?
            .verify(&self.manifest_file.bytes)?;
        Ok(())
    }

    /// Convert the private X: trust anchor into the only task object accepted by destructive
    /// workers.  Public files are treated as authenticated payload replicas/artifacts, never as
    /// operation selectors.
    pub fn into_task(self) -> Result<AuthenticatedOperationTask> {
        ConfigFileManager::authenticate_operation_task(self, |_| {})
    }

    pub fn into_task_with_progress(
        self,
        on_progress: impl FnMut(TaskAuthenticationProgress),
    ) -> Result<AuthenticatedOperationTask> {
        ConfigFileManager::authenticate_operation_task(self, on_progress)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskAuthenticationProgress {
    LocatingVolumes,
    AuthenticatingArtifacts {
        completed_bytes: u64,
        total_bytes: u64,
        current_path: String,
    },
    Finalizing,
}

pub enum AuthenticatedOperationConfig {
    Install(InstallConfig),
    Backup(BackupConfig),
    Expand(ExpandConfig),
}

/// Move-only task authorization. The exact configuration comes only from the private boot WIM;
/// public volumes contribute only independent random locator markers and manifest-bound files.
pub struct AuthenticatedOperationTask {
    guard: AuthenticatedOperationGuard,
    data_volume_root: PathBuf,
    _data_locator: AuthenticatedLocatedVolume,
    install_target: Option<AuthenticatedInstallTarget>,
    install_target_mount: Option<TemporaryLocatorMount>,
    full_disk_targets: Vec<AuthenticatedFullDiskTarget>,
    public_artifacts: Vec<AuthenticatedPublicArtifact>,
    install_image_set: Option<AuthenticatedInstallImageSet>,
    config: AuthenticatedOperationConfig,
}

struct AuthenticatedInstallTarget {
    partition: String,
    expected: lr_core::windows_storage::VolumeIdentity,
    marker: LockedBootPayloadFile,
    temporary_mount: Option<TemporaryLocatorMount>,
}

struct TemporaryLocatorMount {
    letter: char,
    identity: lr_core::windows_storage::VolumeIdentity,
    layout: lr_core::windows_storage::DiskLayoutSnapshot,
    active: bool,
}

impl TemporaryLocatorMount {
    fn remove(mut self) -> Result<()> {
        lr_core::windows_storage::remove_partition_drive_letter_checked(
            self.identity.disk_number,
            self.identity.offset_bytes,
            self.letter,
            &self.layout,
        )?;
        self.active = false;
        Ok(())
    }
}

impl Drop for TemporaryLocatorMount {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Err(error) = lr_core::windows_storage::remove_partition_drive_letter_checked(
            self.identity.disk_number,
            self.identity.offset_bytes,
            self.letter,
            &self.layout,
        ) {
            log::warn!(
                "[HANDOFF] failed to remove temporary locator access path {}: {error}",
                self.letter
            );
        }
    }
}

pub struct AuthenticatedFullDiskTarget {
    pub locator_token: String,
    pub diagnostic_disk_number: u32,
    pub role: lr_core::custom_install::FullDiskRole,
    pub partition: String,
    pub expected: lr_core::windows_storage::VolumeIdentity,
    marker: LockedBootPayloadFile,
}

#[derive(Clone, Debug)]
pub struct FullDiskExecutionTarget {
    pub locator_token: String,
    pub diagnostic_disk_number: u32,
    pub role: lr_core::custom_install::FullDiskRole,
    pub partition: String,
    pub expected: lr_core::windows_storage::VolumeIdentity,
}

struct AuthenticatedLocatedVolume {
    partition: String,
    root: PathBuf,
    expected: lr_core::windows_storage::VolumeIdentity,
    marker: LockedBootPayloadFile,
}

struct AuthenticatedPublicArtifact {
    record: lr_core::handoff_manifest::ArtifactRecord,
    locked: lr_core::install_source_lock::LockedPlainArtifact,
}

struct AuthenticatedInstallImageSet {
    primary: PathBuf,
    ordered_paths: Vec<PathBuf>,
    _secure_directory: Option<File>,
}

impl AuthenticatedInstallImageSet {
    fn verify_unchanged(&self) -> Result<()> {
        if let Some(directory) = &self._secure_directory {
            lr_core::scoped_temp_file::verify_system_administrators_directory_custody(directory)?;
            lr_core::install_source_lock::verify_engine_visible_directory_contains_only(
                &self.ordered_paths,
            )
            .map_err(anyhow::Error::msg)?;
        }
        lr_core::install_source_lock::verify_exact_install_image_span_paths(
            &self.primary,
            &self.ordered_paths,
        )
        .map_err(anyhow::Error::msg)
    }
}

impl AuthenticatedOperationTask {
    pub fn guard(&self) -> &AuthenticatedOperationGuard {
        &self.guard
    }

    pub fn data_volume_root(&self) -> &Path {
        &self.data_volume_root
    }

    pub fn data_volume_identity(&self) -> lr_core::windows_storage::VolumeIdentity {
        self._data_locator.expected
    }

    pub fn data_partition(&self) -> &str {
        &self._data_locator.partition
    }

    pub fn config(&self) -> &AuthenticatedOperationConfig {
        &self.config
    }

    pub fn install_config(&self) -> Result<&InstallConfig> {
        match &self.config {
            AuthenticatedOperationConfig::Install(config) => Ok(config),
            _ => bail!("authenticated handoff is not an install task"),
        }
    }

    pub fn private_wifi_profile_bytes(&self) -> Result<Option<&[u8]>> {
        let config = self.install_config()?;
        self.verify_unchanged()?;
        match (&self.guard.private_wifi_profile, config.migrate_wifi) {
            (Some(profile), true) => Ok(Some(&profile.bytes)),
            (None, false) => Ok(None),
            _ => bail!("authenticated Wi-Fi profile state is inconsistent"),
        }
    }

    /// The random LRPE4 session marker, not cached disk topology, selects the cross-reboot target.
    pub fn install_target(&self) -> Result<(&str, lr_core::windows_storage::VolumeIdentity)> {
        let target = self
            .install_target
            .as_ref()
            .context("authenticated install task has no target marker match")?;
        Ok((&target.partition, target.expected))
    }

    /// Remove the exact marker through its already-locked file object before formatting or writing
    /// the selected volume, which also releases all marker-volume directory handles.
    pub fn release_install_target_marker(&mut self) -> Result<()> {
        let target = self
            .install_target
            .take()
            .context("authenticated installation target marker was already released")?;
        self.install_target_mount = target.temporary_mount;
        target.marker.delete_same_locked_file()
    }

    /// Release every locator handle immediately before the full-disk transaction.
    ///
    /// The selected old partitions are about to be deleted/cleaned, so deleting each marker first
    /// adds no target-safety benefit. A delete failure would instead destroy retryability after
    /// some earlier markers had already disappeared. Closing the already-verified handles is the
    /// only required transition; the subsequent checked topology transaction removes the files
    /// with their old partitions. Returned disk numbers are diagnostics only.
    pub fn release_full_disk_markers(&mut self) -> Result<Vec<FullDiskExecutionTarget>> {
        self.install_config()?;
        if let Some(target) = self.install_target.take() {
            drop(target.marker);
        }
        let targets = std::mem::take(&mut self.full_disk_targets);
        let mut released = Vec::with_capacity(targets.len());
        for target in targets {
            drop(target.marker);
            released.push(FullDiskExecutionTarget {
                locator_token: target.locator_token,
                diagnostic_disk_number: target.diagnostic_disk_number,
                role: target.role,
                partition: target.partition,
                expected: target.expected,
            });
        }
        Ok(released)
    }

    pub fn full_disk_execution_targets(&self) -> Result<Vec<FullDiskExecutionTarget>> {
        self.install_config()?;
        self.verify_unchanged()?;
        Ok(self
            .full_disk_targets
            .iter()
            .map(|target| FullDiskExecutionTarget {
                locator_token: target.locator_token.clone(),
                diagnostic_disk_number: target.diagnostic_disk_number,
                role: target.role,
                partition: target.partition.clone(),
                expected: target.expected,
            })
            .collect())
    }

    /// Return the authenticated public data directory. Every consumed file remains held by this
    /// task with write/delete sharing denied. Keeping directory-shaped inputs on their real volume
    /// avoids copying driver and XP trees into the small X: RAM disk before progress can start.
    pub fn install_data_dir(&self) -> Result<PathBuf> {
        self.install_config()?;
        Ok(self.data_volume_root.join(ConfigFileManager::DATA_DIR))
    }

    /// Resolve the authenticated install source. Large image spans remain on the stable public
    /// volume behind deny-write/delete handles; directory-shaped inputs are consumed only from
    /// the exact-set protected X: snapshot.
    pub fn install_source_path(&self) -> Result<PathBuf> {
        let config = self.install_config()?;
        if config.is_xp_i386 {
            return Ok(self
                .install_data_dir()?
                .join(&config.image_path)
                .join(&config.xp_source_arch));
        }
        let image_set = self
            .install_image_set
            .as_ref()
            .context("authenticated install task has no exact image-set authorization")?;
        image_set.verify_unchanged()?;
        Ok(image_set.primary.clone())
    }

    /// Return the exact manifest-ordered image span paths retained by this move-only task.
    /// Callers must keep the task alive while the external image engine consumes the paths.
    pub fn install_image_span_paths(&self) -> Result<Vec<PathBuf>> {
        let image_set = self
            .install_image_set
            .as_ref()
            .context("authenticated install task has no exact image-set authorization")?;
        image_set.verify_unchanged()?;
        Ok(image_set.ordered_paths.clone())
    }

    pub fn install_artifact_paths(
        &self,
        role: lr_core::handoff_manifest::ArtifactRole,
    ) -> Result<Vec<PathBuf>> {
        self.install_config()?;
        let mut artifacts = self
            .public_artifacts
            .iter()
            .filter(|artifact| artifact.record.role == role)
            .collect::<Vec<_>>();
        artifacts.sort_by_key(|artifact| artifact.record.ordinal);
        artifacts
            .into_iter()
            .map(|artifact| {
                artifact
                    .locked
                    .verify_binding_unchanged()
                    .map_err(anyhow::Error::msg)?;
                Ok(artifact.locked.identity().path.clone())
            })
            .collect()
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        self.guard.verify_unchanged()?;
        self._data_locator
            .marker
            ._ancestor_pins
            .verify_unchanged()?;
        if let Some(target) = &self.install_target {
            target.marker._ancestor_pins.verify_unchanged()?;
        }
        for target in &self.full_disk_targets {
            target.marker._ancestor_pins.verify_unchanged()?;
        }
        for artifact in &self.public_artifacts {
            artifact
                .locked
                .verify_binding_unchanged()
                .map_err(anyhow::Error::msg)?;
        }
        if let Some(image_set) = &self.install_image_set {
            image_set.verify_unchanged()?;
        }
        Ok(())
    }

    /// Delete only this task's exact locator markers through their already-locked kernel objects.
    pub fn cleanup_public_control_files(self) -> Result<()> {
        self.verify_unchanged()?;
        let AuthenticatedOperationTask {
            guard: _,
            data_volume_root: _,
            _data_locator: data_locator,
            install_target,
            install_target_mount,
            full_disk_targets,
            public_artifacts: _,
            install_image_set: _,
            config: _,
        } = self;
        // A no-letter target may have received a current-session PE access path. Remove it through
        // the checked topology binding before deleting retry markers, so a cleanup failure cannot
        // destroy the authorization needed for another attempt.
        if let Some(mount) = install_target_mount {
            mount.remove()?;
        }
        if let Some(target) = install_target {
            if let Some(mount) = target.temporary_mount {
                mount.remove()?;
            }
            target.marker.delete_same_locked_file()?;
        }
        for target in full_disk_targets {
            target.marker.delete_same_locked_file()?;
        }
        data_locator.marker.delete_same_locked_file()
    }

    /// Transfer an authenticated install's optional auto-staging authorization into the checked
    /// storage transaction. When a staging partition exists, all handles on that volume are
    /// verified and then released without deleting the public files: a failed topology operation
    /// therefore leaves the complete LRPE4 retry material intact. A successful partition deletion
    /// removes those files together with the exact authenticated extent.
    pub fn into_install_cleanup_authorization(
        mut self,
    ) -> Result<Option<lr_core::handoff_manifest::AutoStagingAuthorization>> {
        self.install_config()?;
        self.verify_unchanged()?;
        if self.install_target.is_some() {
            self.release_install_target_marker()?;
        }
        if let Some(mount) = self.install_target_mount.take() {
            mount.remove()?;
        }
        let authorization = self.guard.manifest.auto_staging.clone();
        if authorization.is_none() {
            self.cleanup_public_control_files()?;
        }
        Ok(authorization)
    }

    /// Pre-write failure cleanup: remove this session's target/full-disk locators, but keep the
    /// data locator when it resides on an authenticated auto-staging partition so the checked
    /// topology rollback can delete that exact partition. All artifact handles are released when
    /// this move-only task is consumed.
    pub fn into_prewrite_cleanup_authorization(
        mut self,
    ) -> Result<Option<lr_core::handoff_manifest::AutoStagingAuthorization>> {
        self.install_config()?;
        self.verify_unchanged()?;
        let authorization = self.guard.manifest.auto_staging.clone();
        if authorization.is_none() {
            self.cleanup_public_control_files()?;
            return Ok(None);
        }
        if self.install_target.is_some() {
            self.release_install_target_marker()?;
        }
        if let Some(mount) = self.install_target_mount.take() {
            mount.remove()?;
        }
        for target in std::mem::take(&mut self.full_disk_targets) {
            target.marker.delete_same_locked_file()?;
        }
        // The data locator is deliberately retained inside the staging extent and disappears only
        // if the exact authenticated staging cleanup succeeds. Dropping `self` releases every
        // handle before the VDS delete/extend transaction begins.
        Ok(authorization)
    }
}

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

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

fn verify_path_absent(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("read back cleanup target {}", path.display()))
        }
        Ok(_) => bail!(
            "cleanup target still exists after removal: {}",
            path.display()
        ),
    }
}

#[cfg(not(windows))]
fn remove_file_and_verify_absent(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            log::info!("removed handoff marker: {}", path.display());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("remove handoff marker {}", path.display()));
        }
    }
    verify_path_absent(path)
}

/// 驱动操作模式
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DriverActionMode {
    /// 无操作
    #[default]
    None = 0,
    /// 仅保存驱动（到数据目录）
    SaveOnly = 1,
    /// 自动导入（保存并导入到新系统）
    AutoImport = 2,
}

impl DriverActionMode {
    /// 从数值转换
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::SaveOnly,
            2 => Self::AutoImport,
            _ => Self::None,
        }
    }

    /// 是否需要导入驱动
    pub fn should_import(&self) -> bool {
        *self == Self::AutoImport
    }

    /// 是否有驱动目录（SaveOnly 或 AutoImport 时都有）
    pub fn has_drivers(&self) -> bool {
        *self != Self::None
    }
}

/// 系统安装配置（用于PE环境内安装）
#[derive(Debug, Clone, Default)]
pub struct InstallConfig {
    /// 本次安装任务会话ID，用于绑定 marker 与配置。
    pub session_id: String,
    /// 无人值守安装
    pub unattended: bool,
    /// 驱动还原（兼容旧版本）
    pub restore_drivers: bool,
    /// 驱动操作模式: 0=无, 1=仅保存, 2=自动导入
    pub driver_action_mode: DriverActionMode,
    /// 立即重启
    pub auto_reboot: bool,
    /// Authenticated disposable-VM automation policy from the normal endpoint CLI.
    pub automation_shutdown_on_terminal: bool,
    /// 在释放镜像前格式化目标分区。
    pub format_partition: bool,
    /// Preserve six local personal directories, then delete the old system tree without format.
    pub preserve_personal_files: bool,
    /// 在释放镜像后写入或修复目标系统引导。
    pub repair_boot: bool,
    /// 原系统引导GUID（用于删除旧引导项）
    pub original_guid: String,
    /// 安装分卷索引
    pub volume_index: u32,
    /// 目标分区盘符
    pub target_partition: String,
    /// Shared authenticated topology plan parsed from the exact config embedded in this boot WIM.
    pub custom_install_plan: lr_core::custom_install::CustomInstallPlan,
    /// 镜像文件路径（相对于数据分区）
    pub image_path: String,
    /// 是否为GHO格式
    pub is_gho: bool,
    /// The secret-bearing XML remains in the authenticated private boot WIM.
    pub migrate_wifi: bool,
    pub wifi_profile_length: u64,
    pub wifi_profile_sha256: String,
    /// CAB更新包安装: true=安装, false=不安装
    pub install_cab_packages: bool,

    // 高级选项
    /// 移除快捷方式小箭头
    pub remove_shortcut_arrow: bool,
    /// Win11恢复经典右键
    pub restore_classic_context_menu: bool,
    /// OOBE绕过强制联网
    pub bypass_nro: bool,
    /// Remove the audited active Windows Update component surface on Windows 11 build 26100.
    /// The serialized legacy field name is retained for handoff compatibility.
    pub disable_windows_update: bool,
    /// Windows Security UI is distinct from the preserved Security Health/Firewall services.
    /// Remove the Defender Antivirus engine and exactly target the Windows Security UI AppX;
    /// SecurityHealthService, wscsvc, mpssvc, and firewall services remain preserved.
    pub disable_windows_defender: bool,
    /// 禁用系统保留空间
    pub disable_reserved_storage: bool,
    /// 禁用用户账户控制
    pub disable_uac: bool,
    /// 禁用自动设备加密
    pub disable_device_encryption: bool,
    /// 精确删除受支持清单中的预配 AppX；已确认 Win10/11 且使用内置 unattend 时还会调用
    /// Windows 11 还会在默认用户首次生成前关闭推荐和预装内容投递；Outlook 与 OneDrive
    ///（AppX/Win32）明确保留。旧 RemoveUWPApps=true 也收窄为同一语义。
    pub remove_uwp_apps: bool,
    /// 导入磁盘控制器驱动
    pub import_storage_controller_drivers: bool,
    /// 自定义用户名
    pub custom_username: String,
    /// 内置 RID-500 Administrator 的无人值守配置。
    pub builtin_administrator: BuiltInAdministratorOptions,
    /// 自定义系统盘卷标
    pub volume_label: String,
    /// 自定义无人值守文件（数据目录下的相对文件名，空=使用内置生成）
    pub custom_unattend_file: String,
    /// Authenticated URL-safe base64 JSON describing the exact installers and parameterized
    /// silent-install commands selected by the normal endpoint.
    pub preinstalled_software_config: String,

    // Win7 专用选项
    /// Win7 UEFI 补丁（使用 UefiSeven）
    pub win7_uefi_patch: bool,
    /// Win7 注入USB3驱动
    pub win7_inject_usb3_driver: bool,
    /// Win7 注入NVMe驱动
    pub win7_inject_nvme_driver: bool,
    /// Win7 修复ACPI蓝屏
    pub win7_fix_acpi_bsod: bool,
    /// Win7 修复存储控制器蓝屏
    pub win7_fix_storage_bsod: bool,

    /// WIM 镜像引擎：0=libwim（默认），1=wimgapi。由正常系统端随重启传入。
    pub wim_engine: u8,

    /// 目标镜像是否为 XP/2003：为真时写 XP 引导（ntldr/boot.ini 或 UEFI/GPT）而非 bcdboot。
    pub is_xp: bool,

    /// Original I386/AMD64 text-mode media staged as a directory by the desktop client.
    pub is_xp_i386: bool,
    /// Safe single directory component beneath the staged source root (`I386` or `AMD64`).
    pub xp_source_arch: String,

    // XP 专用选项（仅 is_xp 为真时生效）
    /// XP 注入 USB3(xHCI) 驱动（默认勾选）
    pub xp_inject_usb3_driver: bool,
    /// XP 注入 NVMe 驱动（默认勾选）
    pub xp_inject_nvme_driver: bool,

    /// 历史只读兼容字段；为 true 时仅进入旧脚本拒绝守卫，不执行任何脚本。
    pub run_diskpart_scripts: bool,

    /// 引导模式：0=自动，1=UEFI，2=Legacy。
    pub boot_mode: u8,
    /// UEFI Windows Boot Manager 签名选择。
    pub boot_pca_mode: BootPcaMode,
    /// PCA2023 兼容包在数据目录中的安全相对路径；空表示不需要。
    pub pca_compat_package: String,
    /// 暂存兼容包的 SHA-256。
    pub pca_compat_sha256: String,
    /// 兼容包内要提取的 WIM 卷索引。
    pub pca_compat_image_index: u32,
    /// 兼容包绑定的目标 Windows build。
    pub pca_compat_target_build: u32,
    /// 兼容包绑定的目标 WIM architecture 值。
    pub pca_compat_target_architecture: u16,

    /// 界面语言代码（如 "zh-TW"、"en-US"），由正常系统端随重启写入；空=简体中文。
    pub language: String,
}

impl InstallConfig {
    pub fn selected_preinstalled_software(
        &self,
    ) -> Result<Vec<lr_core::software_install::SelectedSoftwarePackage>> {
        if self.preinstalled_software_config.is_empty() {
            Ok(Vec::new())
        } else {
            lr_core::software_install::decode_selected_packages(&self.preinstalled_software_config)
                .context("decode authenticated preinstalled-software selection")
        }
    }

    /// 判断是否需要导入驱动
    /// 优先使用新的driver_action_mode，兼容旧的restore_drivers
    pub fn should_import_drivers(&self) -> bool {
        // 优先使用新的driver_action_mode
        if self.driver_action_mode != DriverActionMode::None {
            self.driver_action_mode.should_import()
        } else {
            // 兼容旧版本
            self.restore_drivers
        }
    }

    /// 判断是否有驱动目录需要处理
    pub fn has_driver_data(&self) -> bool {
        self.driver_action_mode.has_drivers() || self.restore_drivers
    }

    /// Returns true only when the user actually requested an optional offline tweak handled by
    /// `apply_advanced_options`. With every option disabled the post-install phase must be a no-op
    /// and must not load registry hives merely to prove that it could have done work.
    pub fn has_requested_advanced_options(&self) -> bool {
        self.remove_shortcut_arrow
            || self.restore_classic_context_menu
            || self.bypass_nro
            || self.disable_windows_update
            || self.disable_windows_defender
            || self.disable_uac
            || self.disable_device_encryption
            || self.remove_uwp_apps
            || self.import_storage_controller_drivers
            || self.win7_inject_usb3_driver
            || self.win7_inject_nvme_driver
            || self.win7_fix_acpi_bsod
            || self.win7_fix_storage_bsod
    }
}

#[cfg(test)]
mod install_config_option_tests {
    use super::InstallConfig;

    #[test]
    fn disabled_advanced_options_are_a_true_no_op() {
        assert!(!InstallConfig::default().has_requested_advanced_options());
    }

    #[test]
    fn every_supported_advanced_group_arms_the_optional_phase() {
        let config = InstallConfig {
            remove_shortcut_arrow: true,
            ..InstallConfig::default()
        };
        assert!(config.has_requested_advanced_options());

        let config = InstallConfig {
            import_storage_controller_drivers: true,
            ..InstallConfig::default()
        };
        assert!(config.has_requested_advanced_options());

        let config = InstallConfig {
            win7_inject_nvme_driver: true,
            ..InstallConfig::default()
        };
        assert!(config.has_requested_advanced_options());

        let config = InstallConfig {
            remove_uwp_apps: true,
            ..InstallConfig::default()
        };
        assert!(config.has_requested_advanced_options());

        let config = InstallConfig {
            custom_username: "User".to_owned(),
            ..InstallConfig::default()
        };
        assert!(
            !config.has_requested_advanced_options(),
            "account-only unattended input must not load offline registry hives"
        );

        let config = InstallConfig {
            disable_reserved_storage: true,
            ..InstallConfig::default()
        };
        assert!(
            !config.has_requested_advanced_options(),
            "online-only first-logon work must not load offline registry hives"
        );
    }
}

/// 备份格式
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BackupFormat {
    #[default]
    Wim = 0,
    Esd = 1,
    Swm = 2,
    Gho = 3,
}

impl BackupFormat {
    /// 从数值转换
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Wim,
            1 => Self::Esd,
            2 => Self::Swm,
            3 => Self::Gho,
            _ => Self::Wim,
        }
    }
}

/// 系统备份配置（用于PE环境内备份）
#[derive(Debug, Clone, Default)]
pub struct BackupConfig {
    /// 备份名称
    pub name: String,
    /// 备份描述
    pub description: String,
    /// 备份格式
    pub format: BackupFormat,
    /// WIM 镜像引擎：0=libwim（默认），1=wimgapi。由正常系统端随重启传入。
    pub wim_engine: u8,

    /// 界面语言代码（如 "zh-TW"、"en-US"），由正常系统端随重启写入；空=简体中文。
    pub language: String,
    /// LRBK2 stable source/destination/session authorization. Legacy drive-letter-only backup
    /// handoffs are deliberately rejected before capture.
    pub handoff: Option<lr_core::backup_handoff::BackupHandoffV2>,
}

/// 配置文件管理器
pub struct ConfigFileManager;

impl ConfigFileManager {
    fn bind_protected_administrator_secret(
        config: &mut InstallConfig,
        secret: Option<&[u8]>,
    ) -> Result<()> {
        if !config.builtin_administrator.password.is_empty() {
            bail!("public install configuration contains an Administrator password");
        }
        match (config.builtin_administrator.enabled, secret) {
            (true, Some(secret)) => {
                let password =
                    lr_core::unattend_account::parse_protected_administrator_secret(secret)
                        .map_err(anyhow::Error::msg)?;
                config.builtin_administrator.password =
                    lr_core::unattend_account::SensitiveString::new(password.as_str());
                config
                    .builtin_administrator
                    .validate()
                    .context("validate protected built-in Administrator handoff")
            }
            (false, None) => Ok(()),
            (true, None) => {
                bail!("built-in Administrator is enabled without a protected boot secret")
            }
            (false, Some(_)) => {
                bail!("protected Administrator secret exists while the option is disabled")
            }
        }
    }

    pub fn install_config_from_guard(guard: &AuthenticatedOperationGuard) -> Result<InstallConfig> {
        if guard.purpose() != lr_core::handoff_auth::HandoffPurpose::Install {
            bail!("authenticated handoff is not an install task");
        }
        let content = std::str::from_utf8(guard.exact_config_bytes())
            .context("authenticated install configuration is not UTF-8")?;
        Self::deserialize_install_config(content)
    }

    fn find_volume_locator(
        marker_name: &str,
        token: &str,
        role: &str,
    ) -> Result<AuthenticatedLocatedVolume> {
        let expected_bytes = lr_core::install_handoff::locator_marker_bytes(token)?;
        let mut selected: Option<AuthenticatedLocatedVolume> = None;

        // Enumerate the Windows volume namespace itself. Drive letters are mount aliases and a
        // perfectly ordinary basic volume may have none after reboot; limiting discovery to A:..
        // Z: would therefore turn a unique authenticated marker into a false "not found" result.
        for root in lr_core::windows_storage::volume_guid_paths()
            .context("enumerate current volumes for authenticated locator discovery")?
        {
            let path = PathBuf::from(&root).join(marker_name);

            // Same-name files with different contents, malformed files, reparse points and
            // unreadable unrelated volumes are deliberately ignored. Open each candidate once;
            // the retained locked object is the only authority used after selection.
            let Ok(marker) = LockedBootPayloadFile::open(&path, 4096) else {
                continue;
            };
            if marker.bytes.as_slice() != expected_bytes
                || !lr_core::install_handoff::locator_marker_matches(marker.bytes.as_slice(), token)
            {
                continue;
            }
            // Once the full 256-bit token matches, this is no longer unrelated environmental
            // noise. Failing to obtain its current extent means uniqueness cannot be proven; do
            // not silently discard an exact locator and accidentally accept another copy.
            let identity = lr_core::windows_storage::volume_identity_from_guid_path(&root)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("read current extent for exact authenticated {role}"))?;

            if let Some(current) = &selected {
                if lr_core::windows_storage::same_volume_identity(current.expected, identity) {
                    continue;
                }
                bail!("multiple volumes contain the exact authenticated {role} locator");
            }
            let partition = lr_core::windows_storage::assigned_drive_letters_for_partition(
                identity.disk_number,
                identity.offset_bytes,
            )
            .map_err(anyhow::Error::from)
            .with_context(|| format!("read authenticated {role} volume access paths"))?
            .first()
            .map_or_else(
                || root.trim_end_matches('\\').to_owned(),
                |letter| format!("{letter}:"),
            );
            selected = Some(AuthenticatedLocatedVolume {
                partition,
                root: PathBuf::from(root),
                expected: identity,
                marker,
            });
        }

        selected
            .with_context(|| format!("no volume contains the exact authenticated {role} locator"))
    }

    fn find_install_target_marker(
        token: &str,
        require_drive_letter: bool,
    ) -> Result<AuthenticatedInstallTarget> {
        let mut located = Self::find_volume_locator(
            lr_core::install_handoff::INSTALL_TARGET_MARKER_NAME,
            token,
            "installation target",
        )?;
        let temporary_mount = if require_drive_letter && located.partition.starts_with(r"\\?\") {
            let letter = lr_core::boot_pca::find_available_drive_letter()
                .context("authenticated installation target has no access path and no drive letter is available")?;
            let layout =
                lr_core::windows_storage::disk_layout_snapshot(located.expected.disk_number)?;
            lr_core::windows_storage::assign_partition_drive_letter_checked(
                located.expected.disk_number,
                located.expected.offset_bytes,
                letter,
                &layout,
            )?;
            let actual = lr_core::windows_storage::volume_identity(letter)?;
            if !lr_core::windows_storage::same_volume_identity(actual, located.expected) {
                let cleanup = lr_core::windows_storage::remove_partition_drive_letter_checked(
                    located.expected.disk_number,
                    located.expected.offset_bytes,
                    letter,
                    &layout,
                );
                return Err(match cleanup {
                    Ok(()) => anyhow!("temporary target access path resolves to a different extent"),
                    Err(error) => anyhow!(
                        "temporary target access path resolves to a different extent and cleanup failed: {error}"
                    ),
                });
            }
            located.partition = format!("{letter}:");
            Some(TemporaryLocatorMount {
                letter,
                identity: located.expected,
                layout,
                active: true,
            })
        } else {
            None
        };
        Ok(AuthenticatedInstallTarget {
            partition: located.partition,
            expected: located.expected,
            marker: located.marker,
            temporary_mount,
        })
    }

    fn find_full_disk_targets(
        config: &AuthenticatedOperationConfig,
    ) -> Result<Vec<AuthenticatedFullDiskTarget>> {
        let AuthenticatedOperationConfig::Install(config) = config else {
            return Ok(Vec::new());
        };
        let lr_core::custom_install::CustomInstallPlan::RepartitionAllDisks(plan) =
            &config.custom_install_plan
        else {
            return Ok(Vec::new());
        };
        let mut targets = Vec::with_capacity(plan.disks.len());
        for selection in &plan.disks {
            let located = Self::find_volume_locator(
                lr_core::install_handoff::FULL_DISK_MARKER_NAME,
                &selection.locator_token,
                "full-disk target",
            )?;
            if targets
                .iter()
                .any(|existing: &AuthenticatedFullDiskTarget| {
                    lr_core::windows_storage::same_volume_identity(
                        existing.expected,
                        located.expected,
                    )
                })
            {
                bail!("two authenticated full-disk locators resolved to the same current volume");
            }
            targets.push(AuthenticatedFullDiskTarget {
                locator_token: selection.locator_token.clone(),
                diagnostic_disk_number: selection.diagnostic_disk_number,
                role: selection.role,
                partition: located.partition,
                expected: located.expected,
                marker: located.marker,
            });
        }
        Ok(targets)
    }

    fn authenticate_operation_task(
        guard: AuthenticatedOperationGuard,
        mut on_progress: impl FnMut(TaskAuthenticationProgress),
    ) -> Result<AuthenticatedOperationTask> {
        guard.verify_unchanged()?;
        let purpose = guard.purpose();
        let content = std::str::from_utf8(guard.exact_config_bytes())
            .context("authenticated handoff configuration is not UTF-8")?;
        let mut config = match purpose {
            lr_core::handoff_auth::HandoffPurpose::Install => {
                AuthenticatedOperationConfig::Install(Self::deserialize_install_config(content)?)
            }
            lr_core::handoff_auth::HandoffPurpose::Backup => {
                AuthenticatedOperationConfig::Backup(Self::deserialize_backup_config(content)?)
            }
            lr_core::handoff_auth::HandoffPurpose::Expand => {
                AuthenticatedOperationConfig::Expand(Self::deserialize_expand_config(content)?)
            }
            lr_core::handoff_auth::HandoffPurpose::Maintenance => {
                bail!("maintenance handoff does not contain an install/backup/expand task")
            }
        };
        if let AuthenticatedOperationConfig::Install(install) = &mut config {
            Self::bind_protected_administrator_secret(
                install,
                guard
                    .administrator_secret
                    .as_ref()
                    .map(|secret| secret.bytes.as_slice()),
            )?;
        }
        Self::validate_authenticated_manifest_semantics(&config, &guard.manifest)?;
        on_progress(TaskAuthenticationProgress::LocatingVolumes);
        let data_locator = Self::find_volume_locator(
            lr_core::install_handoff::DATA_VOLUME_MARKER_NAME,
            &guard.manifest.data_locator_token,
            "data volume",
        )?;
        let data_volume_root = data_locator.root.clone();
        let full_disk_install = matches!(
            &config,
            AuthenticatedOperationConfig::Install(InstallConfig {
                custom_install_plan:
                    lr_core::custom_install::CustomInstallPlan::RepartitionAllDisks(_),
                ..
            })
        );
        let install_target = match guard.manifest.install_target_token.as_deref() {
            Some(token) => Some(Self::find_install_target_marker(token, !full_disk_install)?),
            None => None,
        };
        let full_disk_targets = Self::find_full_disk_targets(&config)?;

        let total_bytes = guard
            .manifest
            .artifacts
            .iter()
            .filter(|record| {
                record.location == lr_core::handoff_manifest::ArtifactLocation::PublicData
            })
            .try_fold(0_u64, |sum, record| sum.checked_add(record.length_bytes))
            .context("authenticated artifact byte total overflow")?;
        let mut completed_before = 0_u64;
        let mut public_artifacts = Vec::with_capacity(guard.manifest.artifacts.len());
        for record in &guard.manifest.artifacts {
            if record.location != lr_core::handoff_manifest::ArtifactLocation::PublicData {
                continue;
            }
            let current_path = record.relative_path.clone();
            let locked = lr_core::install_source_lock::LockedPlainArtifact::acquire_with_progress(
                &data_volume_root.join(&record.relative_path),
                |current_read| {
                    on_progress(TaskAuthenticationProgress::AuthenticatingArtifacts {
                        completed_bytes: completed_before.saturating_add(current_read),
                        total_bytes,
                        current_path: current_path.clone(),
                    });
                },
            )
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!(
                    "lock authenticated public artifact {}",
                    record.relative_path
                )
            })?;
            let identity = locked.identity();
            if identity.length_bytes != record.length_bytes || identity.sha256 != record.sha256 {
                bail!(
                    "authenticated public artifact changed: {}",
                    record.relative_path
                );
            }
            public_artifacts.push(AuthenticatedPublicArtifact {
                record: record.clone(),
                locked,
            });
            completed_before = completed_before.saturating_add(record.length_bytes);
        }
        guard.verify_unchanged()?;
        for artifact in &public_artifacts {
            artifact
                .locked
                .verify_binding_unchanged()
                .map_err(anyhow::Error::msg)?;
        }
        let install_image_set = Self::authenticate_install_image_set(&config, &public_artifacts)?;
        on_progress(TaskAuthenticationProgress::Finalizing);
        Ok(AuthenticatedOperationTask {
            guard,
            data_volume_root,
            _data_locator: data_locator,
            install_target,
            install_target_mount: None,
            full_disk_targets,
            public_artifacts,
            install_image_set,
            config,
        })
    }

    fn authenticate_install_image_set(
        config: &AuthenticatedOperationConfig,
        artifacts: &[AuthenticatedPublicArtifact],
    ) -> Result<Option<AuthenticatedInstallImageSet>> {
        use lr_core::handoff_manifest::ArtifactRole;

        let AuthenticatedOperationConfig::Install(config) = config else {
            return Ok(None);
        };
        if config.is_xp_i386 {
            return Ok(None);
        }
        let mut spans = artifacts
            .iter()
            .filter(|artifact| artifact.record.role == ArtifactRole::InstallImageSpan)
            .collect::<Vec<_>>();
        spans.sort_by_key(|artifact| artifact.record.ordinal);
        let primary = spans
            .first()
            .context("authenticated install task has no primary image span")?
            .locked
            .identity()
            .path
            .clone();
        let extension = primary
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .context("authenticated install image has no Unicode extension")?;
        if config.is_gho != (extension == "gho") {
            bail!("authenticated image type does not match the install configuration");
        }
        if spans.len() > 1 && !matches!(extension.as_str(), "swm" | "gho") {
            bail!("only SWM and GHO/GHS may contain multiple authenticated image spans");
        }
        let ordered_paths = spans
            .iter()
            .map(|artifact| artifact.locked.identity().path.clone())
            .collect::<Vec<_>>();
        lr_core::install_source_lock::verify_exact_install_image_span_paths(
            &primary,
            &ordered_paths,
        )
        .map_err(anyhow::Error::msg)?;
        let secure_directory = if ordered_paths.len() > 1 {
            let parent = primary
                .parent()
                .context("authenticated split image has no parent directory")?;
            Some(
                lr_core::scoped_temp_file::open_system_administrators_directory(parent)
                    .context("open protected split-image session directory")?,
            )
        } else {
            None
        };
        let authorization = AuthenticatedInstallImageSet {
            primary,
            ordered_paths,
            _secure_directory: secure_directory,
        };
        authorization.verify_unchanged()?;
        Ok(Some(authorization))
    }

    fn validate_authenticated_manifest_semantics(
        config: &AuthenticatedOperationConfig,
        manifest: &lr_core::handoff_manifest::HandoffManifest,
    ) -> Result<()> {
        use lr_core::handoff_manifest::ArtifactRole;

        let count = |role| {
            manifest
                .artifacts
                .iter()
                .filter(|record| record.role == role)
                .count()
        };
        match config {
            AuthenticatedOperationConfig::Install(config) => {
                if config.session_id != manifest.session_id {
                    bail!("install config session does not match authenticated manifest");
                }
                let supported = [
                    ArtifactRole::InstallImageSpan,
                    ArtifactRole::XpSourceFile,
                    ArtifactRole::PreservedDriver,
                    ArtifactRole::UserDriver,
                    ArtifactRole::PcaPackage,
                    ArtifactRole::UefiSevenFile,
                    ArtifactRole::PreinstalledSoftware,
                    ArtifactRole::AutoPartitionMarker,
                    ArtifactRole::ProtectedAdministratorSecret,
                ];
                if let Some(record) = manifest
                    .artifacts
                    .iter()
                    .find(|record| !supported.contains(&record.role))
                {
                    bail!(
                        "install handoff contains an unsupported active artifact role: {:?}",
                        record.role
                    );
                }
                let images = count(ArtifactRole::InstallImageSpan);
                let xp_files = count(ArtifactRole::XpSourceFile);
                let administrator_secrets = count(ArtifactRole::ProtectedAdministratorSecret);
                if config.builtin_administrator.enabled != (administrator_secrets == 1) {
                    bail!(
                        "protected Administrator manifest does not match the authenticated option"
                    );
                }
                if administrator_secrets > 1 {
                    bail!("install handoff has more than one protected Administrator secret");
                }
                if config.is_xp_i386 {
                    if images != 0 || xp_files == 0 {
                        bail!("XP directory install must have only XP source artifacts");
                    }
                    if !matches!(config.xp_source_arch.as_str(), "I386" | "AMD64") {
                        bail!("authenticated XP source architecture is invalid");
                    }
                    let prefix = format!(
                        "{}\\{}\\{}\\",
                        Self::DATA_DIR,
                        config.image_path.trim_end_matches(['\\', '/']),
                        config.xp_source_arch
                    );
                    if manifest
                        .artifacts
                        .iter()
                        .filter(|record| record.role == ArtifactRole::XpSourceFile)
                        .any(|record| !record.relative_path.starts_with(&prefix))
                    {
                        bail!("XP manifest artifact escapes the configured source/architecture");
                    }
                } else {
                    if images == 0 || xp_files != 0 {
                        bail!("PE image install requires one or more authenticated image spans");
                    }
                    if !manifest.artifacts.iter().any(|record| {
                        record.role == ArtifactRole::InstallImageSpan
                            && record.ordinal == 0
                            && record.relative_path
                                == format!("{}\\{}", Self::DATA_DIR, config.image_path)
                    }) {
                        bail!("configured install image is absent from authenticated spans");
                    }
                }

                let pca = manifest
                    .artifacts
                    .iter()
                    .filter(|record| record.role == ArtifactRole::PcaPackage)
                    .collect::<Vec<_>>();
                if config.pca_compat_package.is_empty() {
                    if !pca.is_empty() {
                        bail!("PCA artifact exists while PCA compatibility is disabled");
                    }
                } else {
                    if pca.len() != 1
                        || pca[0].relative_path
                            != format!("{}\\{}", Self::DATA_DIR, config.pca_compat_package)
                    {
                        bail!("PCA config does not bind exactly one matching manifest artifact");
                    }
                    let expected = lr_core::install_handoff::decode_hex_array::<32>(
                        &config.pca_compat_sha256,
                        "PCA compatibility SHA-256",
                    )?;
                    if pca[0].sha256 != expected {
                        bail!("PCA manifest digest does not match the authenticated config");
                    }
                }
                let preserved = count(ArtifactRole::PreservedDriver);
                if config.has_driver_data() != (preserved != 0) {
                    bail!("preserved-driver manifest does not match the authenticated mode");
                }
                let uefiseven = count(ArtifactRole::UefiSevenFile);
                if (config.win7_uefi_patch && config.repair_boot) != (uefiseven != 0) {
                    bail!("UefiSeven manifest does not match the authenticated install options");
                }
                let selected_software = config.selected_preinstalled_software()?;
                let expected_software = selected_software
                    .iter()
                    .map(|package| package.filename.to_ascii_lowercase())
                    .collect::<std::collections::BTreeSet<_>>();
                let software_records = manifest
                    .artifacts
                    .iter()
                    .filter(|record| record.role == ArtifactRole::PreinstalledSoftware)
                    .collect::<Vec<_>>();
                if software_records.len() != expected_software.len() {
                    bail!("preinstalled-software manifest count does not match the authenticated selection");
                }
                let software_prefix = "LetRecovery_Data\\preinstalled_software\\";
                let mut actual_software = std::collections::BTreeSet::new();
                for record in software_records {
                    let filename = record
                        .relative_path
                        .strip_prefix(software_prefix)
                        .filter(|value| !value.is_empty() && !value.contains(['\\', '/']))
                        .ok_or_else(|| {
                            anyhow::anyhow!("preinstalled-software artifact has an unexpected path")
                        })?;
                    if !actual_software.insert(filename.to_ascii_lowercase()) {
                        bail!("preinstalled-software manifest filename appears more than once");
                    }
                }
                if actual_software != expected_software {
                    bail!("preinstalled-software manifest filenames do not match the authenticated selection");
                }
                for (role, prefix) in [
                    (ArtifactRole::PreservedDriver, "LetRecovery_Data\\drivers\\"),
                    (ArtifactRole::UserDriver, "LetRecovery_Data\\user_drivers\\"),
                    (ArtifactRole::UefiSevenFile, "LetRecovery_Data\\uefiseven\\"),
                    (
                        ArtifactRole::PreinstalledSoftware,
                        "LetRecovery_Data\\preinstalled_software\\",
                    ),
                ] {
                    if manifest
                        .artifacts
                        .iter()
                        .filter(|record| record.role == role)
                        .any(|record| !record.relative_path.starts_with(prefix))
                    {
                        bail!("authenticated directory artifact has an unexpected root");
                    }
                }
            }
            AuthenticatedOperationConfig::Backup(config) => {
                let handoff = config
                    .handoff
                    .as_ref()
                    .context("authenticated backup has no LRBK2 authorization")?;
                if handoff.session_id != manifest.session_id {
                    bail!("backup config session does not match authenticated manifest");
                }
                if !manifest.artifacts.is_empty() {
                    bail!("create-only PE backup does not consume public base-image artifacts");
                }
            }
            AuthenticatedOperationConfig::Expand(config) => {
                if config.session_id != manifest.session_id {
                    bail!("expand config session does not match authenticated manifest");
                }
                if !manifest.artifacts.is_empty() {
                    bail!("expand handoff must not contain external artifacts");
                }
            }
        }
        Ok(())
    }
    /// 临时数据目录名
    const DATA_DIR: &'static str = "LetRecovery_Data";

    /// Resolve a file staged by the desktop client without allowing an INI value to escape the
    /// LetRecovery data directory. Staged images and custom unattend files are single files, not
    /// arbitrary relative paths.
    pub fn resolve_staged_file(data_dir: &str, file_name: &str) -> Result<PathBuf> {
        lr_core::download_integrity::validate_download_filename(file_name)
            .map_err(|error| anyhow::anyhow!("无效的暂存文件名 {file_name:?}: {error}"))?;
        let data_dir = Path::new(data_dir);
        let root_metadata = std::fs::symlink_metadata(data_dir)
            .with_context(|| format!("读取暂存目录失败: {}", data_dir.display()))?;
        if !root_metadata.is_dir() || metadata_is_reparse_point(&root_metadata) {
            anyhow::bail!("暂存目录不是普通目录: {}", data_dir.display());
        }
        let path = data_dir.join(file_name);
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("读取暂存文件失败: {}", path.display()))?;
        if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
            anyhow::bail!("暂存输入不是普通文件: {}", path.display());
        }
        Ok(path)
    }

    /// Resolve a two-level staged XP source while keeping both INI-controlled path components
    /// confined to the LetRecovery data directory.
    #[cfg(test)]
    pub fn resolve_staged_xp_source(
        data_dir: &str,
        source_root: &str,
        source_arch: &str,
    ) -> Result<PathBuf> {
        for (field, value) in [("ImagePath", source_root), ("XpSourceArch", source_arch)] {
            lr_core::download_integrity::validate_download_filename(value).map_err(|error| {
                anyhow::anyhow!("无效的 XP 暂存目录字段 {field}={value:?}: {error}")
            })?;
        }
        if !matches!(source_arch.to_ascii_uppercase().as_str(), "I386" | "AMD64") {
            anyhow::bail!("XpSourceArch 必须是 I386 或 AMD64");
        }
        let path = Path::new(data_dir).join(source_root).join(source_arch);
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("读取 XP 暂存源失败: {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!("XP 暂存源不是普通目录: {}", path.display());
        }
        Ok(path)
    }

    /// 反序列化扩容配置
    fn deserialize_expand_config(content: &str) -> Result<ExpandConfig> {
        fn decimal<T>(value: &str, field: &str) -> Result<T>
        where
            T: std::str::FromStr + std::fmt::Display,
            T::Err: std::fmt::Display,
        {
            let parsed = value
                .parse::<T>()
                .map_err(|error| anyhow!("invalid {field}: {error}"))?;
            if parsed.to_string() != value {
                bail!("{field} is not canonical decimal");
            }
            Ok(parsed)
        }

        let mut config = ExpandConfig::default();
        let mut seen = std::collections::HashSet::new();
        if content.starts_with('\u{feff}') || content.replace("\r\n", "").contains(['\r', '\n']) {
            bail!("authenticated expand config has invalid line endings");
        }
        for line in content.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            if matches!(line, "[Expand]" | "[HandoffManifest]") {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .context("authenticated expand config contains a malformed line")?;
            if key.is_empty() || value.trim() != value || !seen.insert(key) {
                bail!("authenticated expand config has an invalid or duplicate {key} field");
            }
            match key {
                "SessionId" => config.session_id = value.to_owned(),
                "TargetPartition" => config.target_partition = value.to_owned(),
                "TargetSizeMb" => config.target_size_mb = decimal(value, key)?,
                "WimEngine" => {
                    config.wim_engine = decimal(value, key)?;
                    if config.wim_engine > 1 {
                        bail!("WimEngine is outside its supported range");
                    }
                }
                "BorrowFromLeft" => {
                    config.borrow_from_left = match value {
                        "true" => true,
                        "false" => false,
                        _ => bail!("BorrowFromLeft must be true or false"),
                    }
                }
                "DonorTargetSizeMb" => config.donor_target_size_mb = decimal(value, key)?,
                "ExpectedDiskNumber" => config.expected_disk_number = decimal(value, key)?,
                "ExpectedDiskSizeBytes" => config.expected_disk_size_bytes = decimal(value, key)?,
                "ExpectedPartitionNumber" => {
                    config.expected_partition_number = decimal(value, key)?
                }
                "ExpectedPartitionOffsetBytes" => {
                    config.expected_partition_offset_bytes = decimal(value, key)?
                }
                "ExpectedPartitionSizeBytes" => {
                    config.expected_partition_size_bytes = decimal(value, key)?
                }
                "ExpectedDonorPartitionNumber" => {
                    config.expected_donor_partition_number = decimal(value, key)?
                }
                "ExpectedDonorOffsetBytes" => {
                    config.expected_donor_offset_bytes = decimal(value, key)?
                }
                "ExpectedDonorSizeBytes" => config.expected_donor_size_bytes = decimal(value, key)?,
                "Language" => config.language = value.to_owned(),
                "HandoffManifestVersion" | "HandoffManifestLength" | "HandoffManifestSha256" => {}
                _ => bail!("authenticated expand config contains unknown field {key}"),
            }
        }
        for required in [
            "SessionId",
            "TargetPartition",
            "TargetSizeMb",
            "WimEngine",
            "BorrowFromLeft",
            "DonorTargetSizeMb",
            "ExpectedDiskNumber",
            "ExpectedDiskSizeBytes",
            "ExpectedPartitionNumber",
            "ExpectedPartitionOffsetBytes",
            "ExpectedPartitionSizeBytes",
            "ExpectedDonorPartitionNumber",
            "ExpectedDonorOffsetBytes",
            "ExpectedDonorSizeBytes",
            "Language",
            "HandoffManifestVersion",
            "HandoffManifestLength",
            "HandoffManifestSha256",
        ] {
            if !seen.contains(required) {
                bail!("authenticated expand config is missing {required}");
            }
        }
        lr_core::handoff_auth::validate_session_id(&config.session_id)?;
        Ok(config)
    }

    /// 获取数据目录路径
    pub fn get_data_dir(partition: &str) -> String {
        format!("{}\\{}", partition, Self::DATA_DIR)
    }

    /// 反序列化安装配置
    fn deserialize_install_config(content: &str) -> Result<InstallConfig> {
        lr_core::install_handoff::validate_install_handoff_ini(content)
            .context("validate installation handoff syntax")?;
        let mut config = InstallConfig {
            volume_index: 1,
            // Older normal-endpoint handoff files always performed both
            // operations and did not contain explicit switches.
            format_partition: true,
            repair_boot: true,
            ..InstallConfig::default()
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "SessionId" => config.session_id = value.to_string(),
                    "Unattended" => config.unattended = value.parse().unwrap_or(false),
                    "RestoreDrivers" => config.restore_drivers = value.parse().unwrap_or(false),
                    "DriverActionMode" => {
                        let mode_value: u8 = value.parse().unwrap_or(0);
                        config.driver_action_mode = DriverActionMode::from_u8(mode_value);
                    }
                    "AutoReboot" => config.auto_reboot = value.parse().unwrap_or(false),
                    "AutomationShutdownOnTerminal" => {
                        config.automation_shutdown_on_terminal =
                            value.parse().with_context(|| {
                                format!("invalid AutomationShutdownOnTerminal boolean: {value}")
                            })?
                    }
                    "FormatPartition" => {
                        config.format_partition = value
                            .parse::<bool>()
                            .with_context(|| format!("invalid FormatPartition boolean: {value}"))?
                    }
                    "PreservePersonalFiles" => {
                        config.preserve_personal_files =
                            value.parse::<bool>().with_context(|| {
                                format!("invalid PreservePersonalFiles boolean: {value}")
                            })?
                    }
                    "RepairBoot" => {
                        config.repair_boot = value
                            .parse::<bool>()
                            .with_context(|| format!("invalid RepairBoot boolean: {value}"))?
                    }
                    "OriginalGUID" => config.original_guid = value.to_string(),
                    "VolumeIndex" => config.volume_index = value.parse().unwrap_or(1),
                    "TargetPartition" => config.target_partition = value.to_string(),
                    "CustomInstallPlanJson" => {
                        config.custom_install_plan =
                            lr_core::custom_install::CustomInstallPlan::from_json(value)
                                .context("parse authenticated custom installation plan")?
                    }
                    "ImagePath" => config.image_path = value.to_string(),
                    "IsGho" => config.is_gho = value.parse().unwrap_or(false),
                    "MigrateWifi" => config.migrate_wifi = value.parse().unwrap_or(false),
                    "WifiProfileLength" => config.wifi_profile_length = value.parse().unwrap_or(0),
                    "WifiProfileSha256" => config.wifi_profile_sha256 = value.to_string(),
                    "WimEngine" => config.wim_engine = value.parse().unwrap_or(0),
                    "IsXp" => config.is_xp = value.parse().unwrap_or(false),
                    "IsXpI386" => config.is_xp_i386 = value.parse().unwrap_or(false),
                    "XpSourceArch" => config.xp_source_arch = value.to_string(),
                    "RunDiskpartScripts" => {
                        config.run_diskpart_scripts = value.parse().unwrap_or(false)
                    }
                    "BootMode" => config.boot_mode = value.parse().unwrap_or(0),
                    "BootPcaMode" => config.boot_pca_mode = BootPcaMode::from_config_value(value),
                    "PcaCompatPackage" => config.pca_compat_package = value.to_string(),
                    "PcaCompatSha256" => config.pca_compat_sha256 = value.to_string(),
                    "PcaCompatImageIndex" => {
                        config.pca_compat_image_index = value.parse().unwrap_or(0)
                    }
                    "PcaCompatTargetBuild" => {
                        config.pca_compat_target_build = value.parse().unwrap_or(0)
                    }
                    "PcaCompatTargetArchitecture" => {
                        config.pca_compat_target_architecture = value.parse().unwrap_or(0)
                    }
                    "Language" => config.language = value.to_string(),
                    "InstallCabPackages" => {
                        config.install_cab_packages = value.parse().unwrap_or(false)
                    }
                    "RemoveShortcutArrow" => {
                        config.remove_shortcut_arrow = value.parse().unwrap_or(false)
                    }
                    "RestoreClassicContextMenu" => {
                        config.restore_classic_context_menu = value.parse().unwrap_or(false)
                    }
                    "BypassNRO" => config.bypass_nro = value.parse().unwrap_or(false),
                    "DisableWindowsUpdate" => {
                        config.disable_windows_update = value.parse().unwrap_or(false)
                    }
                    "DisableWindowsDefender" => {
                        config.disable_windows_defender = value.parse().unwrap_or(false)
                    }
                    "DisableReservedStorage" => {
                        config.disable_reserved_storage = value.parse().unwrap_or(false)
                    }
                    "DisableUAC" => config.disable_uac = value.parse().unwrap_or(false),
                    "DisableDeviceEncryption" => {
                        config.disable_device_encryption = value.parse().unwrap_or(false)
                    }
                    "RemoveUWPApps" => config.remove_uwp_apps = value.parse().unwrap_or(false),
                    "ImportStorageControllerDrivers" => {
                        config.import_storage_controller_drivers = value.parse().unwrap_or(false)
                    }
                    "CustomUsername" => config.custom_username = value.to_string(),
                    "BuiltinAdministrator" => {
                        config.builtin_administrator.enabled = value.parse().unwrap_or(false)
                    }
                    "BuiltinAdministratorName" => {
                        config.builtin_administrator.account_name = value.to_string()
                    }
                    "BuiltinAdministratorPassword" => {
                        config.builtin_administrator.password = value.into()
                    }
                    "BuiltinAdministratorAutoLogon" => {
                        config.builtin_administrator.auto_logon = value.parse().unwrap_or(false)
                    }
                    "VolumeLabel" => config.volume_label = value.to_string(),
                    "CustomUnattendFile" => config.custom_unattend_file = value.to_string(),
                    "PreinstalledSoftwareConfig" => {
                        config.preinstalled_software_config = value.to_string()
                    }
                    "Win7UefiPatch" => config.win7_uefi_patch = value.parse().unwrap_or(false),
                    "Win7InjectUsb3Driver" => {
                        config.win7_inject_usb3_driver = value.parse().unwrap_or(false)
                    }
                    "Win7InjectNvmeDriver" => {
                        config.win7_inject_nvme_driver = value.parse().unwrap_or(false)
                    }
                    // Historical opt-in processor-power workaround. It is not an ACPI table patch,
                    // but remains available for compatibility with explicitly selected Win7 jobs.
                    "Win7FixAcpiBsod" => config.win7_fix_acpi_bsod = value.parse().unwrap_or(false),
                    "Win7FixStorageBsod" => config.win7_fix_storage_bsod = false,
                    "XpInjectUsb3Driver" => {
                        config.xp_inject_usb3_driver = value.parse().unwrap_or(false)
                    }
                    "XpInjectNvmeDriver" => {
                        config.xp_inject_nvme_driver = value.parse().unwrap_or(false)
                    }
                    _ => {}
                }
            }
        }

        let expected_wifi = lr_core::first_logon::private_wifi_binding_from_install_ini(content)?;
        match expected_wifi {
            Some(binding) => {
                if !config.migrate_wifi
                    || config.wifi_profile_length != binding.length_bytes
                    || config.wifi_profile_sha256 != binding.sha256
                {
                    bail!("Wi-Fi binding fields were not parsed consistently");
                }
            }
            None => {
                config.migrate_wifi = false;
                config.wifi_profile_length = 0;
                config.wifi_profile_sha256.clear();
            }
        }
        if config.preserve_personal_files {
            if config.format_partition {
                bail!("personal-file preservation conflicts with target formatting");
            }
            if config.is_gho || config.is_xp || config.is_xp_i386 {
                bail!("personal-file preservation requires a Windows 7+ WIM/ESD/SWM source");
            }
            if config.custom_install_plan.mode()
                != lr_core::custom_install::CustomInstallMode::ReinstallPartition
            {
                bail!("personal-file preservation only supports partition reinstall");
            }
        }
        Ok(config)
    }

    /// 反序列化备份配置
    fn deserialize_backup_config(content: &str) -> Result<BackupConfig> {
        let (values, handoff) = lr_core::backup_handoff::parse_backup_payload(content)?;
        Ok(BackupConfig {
            name: values.name,
            description: values.description,
            format: BackupFormat::from_u8(values.format),
            wim_engine: values.wim_engine,
            language: values.language,
            handoff: Some(handoff),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_install_semantics_accept_contiguous_multi_span_manifest() {
        use lr_core::handoff_auth::HandoffPurpose;
        use lr_core::handoff_manifest::{
            ArtifactLocation, ArtifactRecord, ArtifactRole, HandoffManifest,
        };

        let session = "11111111111111111111111111111111";
        let config = InstallConfig {
            session_id: session.to_owned(),
            image_path: "install-image-set-session\\install.swm".to_owned(),
            ..InstallConfig::default()
        };
        let artifacts = ["install.swm", "install2.swm"]
            .into_iter()
            .enumerate()
            .map(|(ordinal, name)| ArtifactRecord {
                role: ArtifactRole::InstallImageSpan,
                location: ArtifactLocation::PublicData,
                ordinal: ordinal as u32,
                relative_path: format!("LetRecovery_Data\\install-image-set-session\\{name}"),
                length_bytes: 3,
                sha256: [ordinal as u8 + 1; 32],
            })
            .collect();
        let manifest = HandoffManifest::new(
            HandoffPurpose::Install,
            session,
            "1111111111111111111111111111111111111111111111111111111111111111",
            Some("2222222222222222222222222222222222222222222222222222222222222222".to_owned()),
            None,
            artifacts,
        )
        .unwrap();
        ConfigFileManager::validate_authenticated_manifest_semantics(
            &AuthenticatedOperationConfig::Install(config),
            &manifest,
        )
        .unwrap();
    }

    #[test]
    fn old_install_config_defaults_to_auto_boot_selection() {
        let config = ConfigFileManager::deserialize_install_config(
            "[Install]\r\nVolumeIndex=4\r\nTargetPartition=C:\r\n",
        )
        .unwrap();

        assert_eq!(config.volume_index, 4);
        assert!(config.format_partition);
        assert!(config.repair_boot);
        assert_eq!(config.boot_mode, 0);
        assert_eq!(config.boot_pca_mode, BootPcaMode::Auto);
        assert!(config.pca_compat_package.is_empty());
        assert_eq!(config.pca_compat_image_index, 0);
        assert!(!config.is_xp_i386);
        assert!(config.xp_source_arch.is_empty());
        assert!(!config.migrate_wifi);
    }

    #[test]
    fn private_wifi_binding_is_required_as_a_complete_set() {
        let profile = b"<WLANProfile><name>test</name></WLANProfile>";
        let binding = lr_core::first_logon::PrivateWifiProfileBinding::from_bytes(profile).unwrap();
        let config = ConfigFileManager::deserialize_install_config(&format!(
            "[Install]\r\nMigrateWifi=true\r\nWifiProfileLength={}\r\nWifiProfileSha256={}\r\n",
            binding.length_bytes, binding.sha256
        ))
        .unwrap();
        assert!(config.migrate_wifi);
        assert_eq!(config.wifi_profile_length, binding.length_bytes);
        assert!(ConfigFileManager::deserialize_install_config(
            "[Install]\r\nMigrateWifi=true\r\nWifiProfileLength=12\r\n"
        )
        .is_err());
    }

    #[test]
    fn reads_and_validates_preinstalled_software_selection_from_normal_endpoint() {
        let packages = [lr_core::software_install::SelectedSoftwarePackage {
            id: "tool".to_owned(),
            name: "Tool".to_owned(),
            download_url: "https://example.com/tool.msi".to_owned(),
            filename: "tool.msi".to_owned(),
            silent_command: r#"msiexec.exe /i "{installer}" /qn"#.to_owned(),
            requires_admin: true,
        }];
        let encoded = lr_core::software_install::encode_selected_packages(&packages).unwrap();
        let config = ConfigFileManager::deserialize_install_config(&format!(
            "[Install]\r\nVolumeIndex=1\r\n[Advanced]\r\nPreinstalledSoftwareConfig={encoded}\r\n"
        ))
        .unwrap();
        assert_eq!(config.selected_preinstalled_software().unwrap(), packages);
    }

    #[test]
    fn reads_authenticated_automation_terminal_power_policy() {
        let config = ConfigFileManager::deserialize_install_config(
            "[Install]\r\nVolumeIndex=1\r\nAutomationShutdownOnTerminal=true\r\n",
        )
        .unwrap();
        assert!(config.automation_shutdown_on_terminal);
        assert!(ConfigFileManager::deserialize_install_config(
            "[Install]\r\nAutomationShutdownOnTerminal=maybe\r\n"
        )
        .is_err());
    }

    #[test]
    fn rejects_invalid_preinstalled_software_config_before_install() {
        let config = ConfigFileManager::deserialize_install_config(
            "[Install]\r\nVolumeIndex=1\r\n[Advanced]\r\nPreinstalledSoftwareConfig=not-base64!\r\n",
        )
        .unwrap();
        assert!(config.selected_preinstalled_software().is_err());
    }

    #[test]
    fn explicit_invalid_destructive_switches_fail_closed() {
        for content in [
            "[Install]\r\nFormatPartition=fasle\r\n",
            "[Install]\r\nRepairBoot=garbage\r\n",
        ] {
            assert!(ConfigFileManager::deserialize_install_config(content).is_err());
        }
    }

    #[test]
    fn reads_explicit_pca2023_selection_from_normal_endpoint() {
        let config = ConfigFileManager::deserialize_install_config(concat!(
            "[Install]\r\nFormatPartition=false\r\nRepairBoot=false\r\n",
            "BootMode=1\r\nBootPcaMode=pca2023\r\n",
            "PcaCompatPackage=pca_compat\\package.wim\r\n",
            "PcaCompatSha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n",
            "PcaCompatImageIndex=1\r\nPcaCompatTargetBuild=19045\r\n",
            "PcaCompatTargetArchitecture=9\r\n"
        ))
        .unwrap();

        assert!(!config.format_partition);
        assert!(!config.repair_boot);
        assert_eq!(config.boot_mode, 1);
        assert_eq!(config.boot_pca_mode, BootPcaMode::Pca2023);
        assert_eq!(config.pca_compat_package, "pca_compat\\package.wim");
        assert_eq!(config.pca_compat_image_index, 1);
        assert_eq!(config.pca_compat_target_build, 19045);
        assert_eq!(config.pca_compat_target_architecture, 9);
    }

    #[test]
    fn win7_uefi_and_processor_workarounds_are_preserved_but_storage_hack_is_ignored() {
        let config = ConfigFileManager::deserialize_install_config(concat!(
            "[Install]\r\n",
            "Win7UefiPatch=true\r\n",
            "Win7FixAcpiBsod=true\r\n",
            "Win7FixStorageBsod=true\r\n"
        ))
        .unwrap();

        assert!(config.win7_uefi_patch);
        assert!(config.win7_fix_acpi_bsod);
        assert!(!config.win7_fix_storage_bsod);
    }

    #[test]
    fn reads_builtin_administrator_session_settings_from_normal_endpoint() {
        let config = ConfigFileManager::deserialize_install_config(concat!(
            "[Install]\r\n",
            "BuiltinAdministrator=true\r\n",
            "BuiltinAdministratorName=LocalAdmin\r\n",
            "BuiltinAdministratorPassword=temporary-secret\r\n",
            "BuiltinAdministratorAutoLogon=true\r\n"
        ))
        .unwrap();

        assert!(config.builtin_administrator.enabled);
        assert_eq!(config.builtin_administrator.account_name, "LocalAdmin");
        assert_eq!(
            config.builtin_administrator.password.expose_secret(),
            "temporary-secret"
        );
        assert!(config.builtin_administrator.auto_logon);
    }

    #[test]
    fn binds_only_a_private_canonical_administrator_secret() {
        let mut config = ConfigFileManager::deserialize_install_config(concat!(
            "[Install]\r\n",
            "BuiltinAdministrator=true\r\n",
            "BuiltinAdministratorName=LocalAdmin\r\n",
            "BuiltinAdministratorPassword=\r\n",
            "BuiltinAdministratorAutoLogon=true\r\n"
        ))
        .unwrap();
        let secret = lr_core::unattend_account::serialize_protected_administrator_secret(
            &lr_core::unattend_account::SensitiveString::new("temporary-secret"),
        )
        .unwrap();

        ConfigFileManager::bind_protected_administrator_secret(&mut config, Some(&secret)).unwrap();
        assert_eq!(
            config.builtin_administrator.password.expose_secret(),
            "temporary-secret"
        );

        let mut missing = config.clone();
        missing.builtin_administrator.password.clear();
        assert!(
            ConfigFileManager::bind_protected_administrator_secret(&mut missing, None).is_err()
        );

        let mut disabled = InstallConfig::default();
        assert!(ConfigFileManager::bind_protected_administrator_secret(
            &mut disabled,
            Some(&secret)
        )
        .is_err());

        let mut public_password = config;
        assert!(ConfigFileManager::bind_protected_administrator_secret(
            &mut public_password,
            Some(&secret)
        )
        .is_err());
    }

    #[test]
    fn resolves_only_confined_staged_xp_source_directories() {
        let root = std::env::temp_dir().join(format!(
            "letrecovery-pe-xp-resolver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("xp-source-session").join("I386");
        std::fs::create_dir_all(&source).unwrap();
        assert_eq!(
            ConfigFileManager::resolve_staged_xp_source(
                &root.to_string_lossy(),
                "xp-source-session",
                "I386"
            )
            .unwrap(),
            source
        );
        assert!(
            ConfigFileManager::resolve_staged_xp_source(&root.to_string_lossy(), "..", "I386")
                .is_err()
        );
        assert!(ConfigFileManager::resolve_staged_xp_source(
            &root.to_string_lossy(),
            "xp-source-session",
            "system32"
        )
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// 操作类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OperationType {
    Install,
    Backup,
    Expand,
}

/// 无损扩容配置（进 PE 后无损扩大目标分区，通常为系统盘 C:）。
#[derive(Debug, Clone, Default)]
pub struct ExpandConfig {
    /// CNG-generated LRPE4 session identifier authenticated by the boot capsule.
    pub session_id: String,
    /// 要扩大的目标分区（如 "C:"）。
    pub target_partition: String,
    /// 期望的最终总大小（MB）；0 表示尽可能扩到最大。
    pub target_size_mb: u64,
    /// WIM 引擎选择（与其它流程一致）：0=libwim，1=wimgapi。
    pub wim_engine: u8,
    /// 是否从目标分区左侧相邻数据分区借用空间并左移目标分区。
    pub borrow_from_left: bool,
    /// 相邻转移中供体分区的精确最终大小；旧配置缺失时为 0。
    pub donor_target_size_mb: u64,
    /// 正常端在重启前保存的磁盘/分区几何；新左侧转移缺失任一字段时失败关闭。
    pub expected_disk_number: u32,
    pub expected_disk_size_bytes: u64,
    pub expected_partition_number: u32,
    pub expected_partition_offset_bytes: u64,
    pub expected_partition_size_bytes: u64,
    pub expected_donor_partition_number: u32,
    pub expected_donor_offset_bytes: u64,
    pub expected_donor_size_bytes: u64,
    /// 界面语言代码（如 "zh-TW"、"en-US"），由正常系统端随重启写入；空=简体中文。
    pub language: String,
}

#[cfg(test)]
mod expand_config_tests {
    use super::ConfigFileManager;

    #[test]
    fn unsigned_legacy_expand_config_is_not_accepted_as_an_authenticated_task() {
        assert!(ConfigFileManager::deserialize_expand_config(
            "[Expand]\r\nTargetPartition=C:\r\nTargetSizeMb=102400\r\nWimEngine=0\r\n",
        )
        .is_err());
    }

    #[test]
    fn expand_config_reads_left_side_donor_flag() {
        let config = ConfigFileManager::deserialize_expand_config(
            "[Expand]\r\nSessionId=0123456789abcdef0123456789abcdef\r\nTargetPartition=E:\r\nTargetSizeMb=204800\r\nWimEngine=0\r\nBorrowFromLeft=true\r\nDonorTargetSizeMb=153600\r\nExpectedDiskNumber=2\r\nExpectedDiskSizeBytes=1000000\r\nExpectedPartitionNumber=4\r\nExpectedPartitionOffsetBytes=600000\r\nExpectedPartitionSizeBytes=200000\r\nExpectedDonorPartitionNumber=3\r\nExpectedDonorOffsetBytes=200000\r\nExpectedDonorSizeBytes=400000\r\nLanguage=zh-CN\r\n[HandoffManifest]\r\nHandoffManifestVersion=1\r\nHandoffManifestLength=123\r\nHandoffManifestSha256=0000000000000000000000000000000000000000000000000000000000000000\r\n",
        )
        .unwrap();
        assert!(config.borrow_from_left);
        assert_eq!(config.donor_target_size_mb, 153_600);
        assert_eq!(config.expected_disk_number, 2);
        assert_eq!(config.expected_partition_number, 4);
        assert_eq!(config.expected_donor_partition_number, 3);
        assert_eq!(config.expected_partition_offset_bytes, 600_000);
        assert_eq!(config.expected_donor_size_bytes, 400_000);
    }
}
