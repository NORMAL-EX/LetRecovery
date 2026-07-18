//! Production side-effect backend for the native direct-install executor.
//!
//! The phase ordering and fail-closed gates live in `native_install_executor`;
//! this module reuses the established image, XP, boot and advanced-option
//! implementations. Desktop-to-PE staging includes both regular image files
//! and session-isolated XP/2003 text-mode source directories.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use lr_core::cached_artifact::CachedArtifactStatus;
use lr_core::command::{CommandExecutor, SystemCommandExecutor};
use lr_core::pca_compat::PreparedPcaCompatPackage;

use super::disk::{DiskManager, Partition, PartitionStyle};
use super::native_install_compat::{
    self, DefaultUnattendOptions, MbrSignatureObservation, PartitionIdentity, UnattendArchitecture,
};
#[cfg(any(not(feature = "non-elevated-tests"), test))]
use super::native_install_controller::InstallMode;
use super::native_install_controller::{PcaCompatConfig, StartInstallIntent};
use super::native_install_executor::{
    InstallBackendError, InstallCancellation, InstallExecutionBackend, InstallExecutionContext,
    InstallExecutionEvent, InstallExecutionPhase, InstallExecutionReporter,
};
use super::ui_state::{BootModeSelection, DriverAction};

const UNSUPPORTED_PENDING: &str = "unsupported_pending";

/// Stateful backend used for one executor run.
///
/// Target identity is resolved again from `disk:partition` before every write
/// branch that depends on a drive letter.  This prevents a DiskPart script or
/// WinPE drive-letter reassignment from redirecting the install.
pub struct ProductionInstallBackend {
    target: String,
    target_style: PartitionStyle,
    partitions: Vec<Partition>,
    pca_package: Option<PreparedPcaCompatPackage>,
    driver_backup: PathBuf,
    pe_path: Option<PathBuf>,
    pe_display_name: Option<String>,
    data_partition: Option<String>,
    staged_image_name: Option<String>,
    staged_xp_source_arch: Option<String>,
    bitlocker_decryption_volumes: Vec<char>,
}

impl ProductionInstallBackend {
    pub fn new(intent: &StartInstallIntent) -> Self {
        Self {
            target: intent.target_partition.clone(),
            target_style: PartitionStyle::Unknown,
            partitions: Vec::new(),
            pca_package: None,
            driver_backup: std::env::temp_dir().join("LetRecovery_DriverBackup"),
            pe_path: None,
            pe_display_name: None,
            data_partition: None,
            staged_image_name: None,
            staged_xp_source_arch: None,
            bitlocker_decryption_volumes: Vec::new(),
        }
    }

    fn error(code: &'static str, error: impl std::fmt::Display) -> InstallBackendError {
        InstallBackendError::new(code, error.to_string())
    }

    const fn supports_direct_phase(phase: InstallExecutionPhase) -> bool {
        matches!(
            phase,
            InstallExecutionPhase::InspectBitLocker
                | InstallExecutionPhase::AwaitBitLockerDecryption
                | InstallExecutionPhase::VerifyPcaBeforeDiskWrite
                | InstallExecutionPhase::ResolveStableTarget
                | InstallExecutionPhase::RunDiskpartScripts
                | InstallExecutionPhase::ResolveTargetAfterDiskpart
                | InstallExecutionPhase::FormatTarget
                | InstallExecutionPhase::ExportHostDrivers
                | InstallExecutionPhase::ApplyXpTextModeSource
                | InstallExecutionPhase::ApplyGhostImage
                | InstallExecutionPhase::ApplyWimImage
                | InstallExecutionPhase::ProcessDrivers
                | InstallExecutionPhase::RepairBoot
                | InstallExecutionPhase::ApplyAdvancedOptions
                | InstallExecutionPhase::FinishDirectInstall
        )
    }

    const fn supports_via_pe_phase(phase: InstallExecutionPhase) -> bool {
        matches!(
            phase,
            InstallExecutionPhase::InspectBitLocker
                | InstallExecutionPhase::AwaitBitLockerDecryption
                | InstallExecutionPhase::VerifyPcaBeforeDiskWrite
                | InstallExecutionPhase::VerifyPeEnvironment
                | InstallExecutionPhase::InstallPeBootEntry
                | InstallExecutionPhase::SelectDataPartition
                | InstallExecutionPhase::PersistPcaCompatibilityPackage
                | InstallExecutionPhase::ExportDriversToPeData
                | InstallExecutionPhase::VerifySourceImage
                | InstallExecutionPhase::CopySourceImage
                | InstallExecutionPhase::StageUefiSeven
                | InstallExecutionPhase::StageUserDrivers
                | InstallExecutionPhase::WritePeInstallConfig
                | InstallExecutionPhase::ReadyToRebootIntoPe
        )
    }

    fn data_partition(&self) -> Result<&str, InstallBackendError> {
        self.data_partition.as_deref().ok_or_else(|| {
            InstallBackendError::new(
                "data_partition_missing",
                "PE data partition is not selected",
            )
        })
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn begin_bitlocker_fallback_decryption(&mut self) -> Result<(), InstallBackendError> {
        self.partitions = DiskManager::get_partitions()
            .map_err(|error| Self::error("bitlocker_inventory", error))?;
        self.bitlocker_decryption_volumes.clear();
        let manager = super::bitlocker::BitLockerManager::new();
        for partition in &self.partitions {
            let Some(letter) = partition.letter.chars().next() else {
                continue;
            };
            let drive = format!("{}:", letter.to_ascii_uppercase());
            match manager.get_status(letter) {
                super::bitlocker::VolumeStatus::NotEncrypted => {}
                super::bitlocker::VolumeStatus::Decrypting => {
                    self.bitlocker_decryption_volumes
                        .push(letter.to_ascii_uppercase());
                }
                super::bitlocker::VolumeStatus::EncryptedUnlocked => {
                    let result = manager.decrypt(&drive);
                    if !result.success {
                        return Err(InstallBackendError::new(
                            "bitlocker_decrypt_start",
                            format!("{drive}: {}", result.message),
                        ));
                    }
                    self.bitlocker_decryption_volumes
                        .push(letter.to_ascii_uppercase());
                }
                super::bitlocker::VolumeStatus::EncryptedLocked => {
                    log::warn!(
                        "[NATIVE INSTALL] skipping locked non-target BitLocker volume {drive} during fallback decryption"
                    );
                }
                super::bitlocker::VolumeStatus::Encrypting => {
                    return Err(InstallBackendError::new(
                        "bitlocker_encrypting",
                        format!("{drive} is still encrypting"),
                    ));
                }
                super::bitlocker::VolumeStatus::Unknown => {
                    return Err(InstallBackendError::new(
                        "bitlocker_status_unknown",
                        format!("cannot determine BitLocker status for {drive}"),
                    ));
                }
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn await_bitlocker_fallback_decryption(
        &mut self,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let manager = super::bitlocker::BitLockerManager::new();
        loop {
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "installation cancelled while waiting for BitLocker decryption",
                ));
            }
            let mut all_decrypted = true;
            let mut highest_encrypted = 0.0_f32;
            for &letter in &self.bitlocker_decryption_volumes {
                let drive = format!("{}:", letter.to_ascii_uppercase());
                let (status, encrypted_percentage) = manager.get_status_with_percentage(letter);
                match status {
                    super::bitlocker::VolumeStatus::NotEncrypted => {}
                    super::bitlocker::VolumeStatus::Decrypting
                    | super::bitlocker::VolumeStatus::EncryptedUnlocked => {
                        all_decrypted = false;
                        highest_encrypted = highest_encrypted.max(encrypted_percentage);
                    }
                    super::bitlocker::VolumeStatus::EncryptedLocked => {
                        return Err(InstallBackendError::new(
                            "bitlocker_relocked",
                            format!("{drive} became locked while decrypting"),
                        ));
                    }
                    super::bitlocker::VolumeStatus::Encrypting => {
                        return Err(InstallBackendError::new(
                            "bitlocker_encrypting",
                            format!("{drive} is encrypting instead of decrypting"),
                        ));
                    }
                    super::bitlocker::VolumeStatus::Unknown => {
                        return Err(InstallBackendError::new(
                            "bitlocker_status_unknown",
                            format!("cannot determine BitLocker status for {drive}"),
                        ));
                    }
                }
            }
            if all_decrypted {
                return Ok(());
            }
            reporter.report(InstallExecutionEvent::Progress {
                phase: InstallExecutionPhase::AwaitBitLockerDecryption,
                percentage: (100.0 - highest_encrypted).clamp(0.0, 100.0) as u8,
                detail: crate::tr!("正在等待 BitLocker 完全解密..."),
            });
            for _ in 0..8 {
                if cancellation.is_cancelled() {
                    return Err(InstallBackendError::new(
                        "cancelled",
                        "installation cancelled while waiting for BitLocker decryption",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn inspect_bitlocker_fresh(
        &mut self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let partitions = DiskManager::get_partitions()
            .map_err(|error| Self::error("bitlocker_inventory", error))?;
        let target = if let Some(identity) = context.stable_target {
            partitions.iter().find(|partition| {
                partition.disk_number == Some(identity.disk_number)
                    && partition.partition_number == Some(identity.partition_number)
            })
        } else {
            partitions.iter().find(|partition| {
                partition
                    .letter
                    .eq_ignore_ascii_case(&intent.target_partition)
            })
        }
        .ok_or_else(|| {
            InstallBackendError::new(
                "bitlocker_target_missing",
                "the verified installation target is no longer present",
            )
        })?;
        let letter = target.letter.chars().next().ok_or_else(|| {
            InstallBackendError::new(
                "bitlocker_target_letter_missing",
                "the verified installation target has no drive letter",
            )
        })?;
        let drive = format!("{}:", letter.to_ascii_uppercase());
        let manager = super::bitlocker::BitLockerManager::new();
        match manager.get_status(letter) {
            super::bitlocker::VolumeStatus::NotEncrypted => Ok(()),
            super::bitlocker::VolumeStatus::EncryptedLocked => Err(InstallBackendError::new(
                "bitlocker_target_locked",
                format!("{drive} is locked; unlock it before installation"),
            )),
            super::bitlocker::VolumeStatus::Unknown => Err(InstallBackendError::new(
                "bitlocker_status_unknown",
                format!("cannot determine BitLocker status for {drive}"),
            )),
            super::bitlocker::VolumeStatus::Encrypting => Err(InstallBackendError::new(
                "bitlocker_target_encrypting",
                format!("{drive} is currently encrypting"),
            )),
            super::bitlocker::VolumeStatus::Decrypting => {
                self.begin_bitlocker_fallback_decryption()?;
                self.await_bitlocker_fallback_decryption(reporter, cancellation)
            }
            super::bitlocker::VolumeStatus::EncryptedUnlocked => {
                if manager.get_recovery_key(&drive).is_ok() {
                    Ok(())
                } else {
                    self.begin_bitlocker_fallback_decryption()?;
                    self.await_bitlocker_fallback_decryption(reporter, cancellation)
                }
            }
        }
    }

    fn data_dir(&self) -> Result<String, InstallBackendError> {
        Ok(super::install_config::ConfigFileManager::get_data_dir(
            self.data_partition()?,
        ))
    }

    fn require_cached_pe(
        status: CachedArtifactStatus,
        filename: &str,
    ) -> Result<PathBuf, InstallBackendError> {
        match status {
            CachedArtifactStatus::Ready { path, .. } => Ok(path),
            CachedArtifactStatus::Missing => Err(InstallBackendError::new(
                "pe_download_required",
                format!(
                    "PE file {filename} is missing; schedule the existing verified download workflow first"
                ),
            )),
        }
    }

    fn verify_pe_environment(
        &mut self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let selected = intent.selected_pe.ok_or_else(|| {
            InstallBackendError::new("missing_pe_selection", "no PE environment was selected")
        })?;
        let entries = crate::download::config::PeCache::load().ok_or_else(|| {
            InstallBackendError::new(
                "pe_catalog_missing",
                "the cached PE catalog is unavailable; refresh the online PE list",
            )
        })?;
        let pe = entries.get(selected).ok_or_else(|| {
            InstallBackendError::new(
                "invalid_pe_selection",
                "selected PE index is no longer valid",
            )
        })?;
        let status = super::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        )
        .map_err(|error| Self::error("pe_cache_rejected", error))?;
        self.pe_path = Some(Self::require_cached_pe(status, &pe.filename)?);
        self.pe_display_name = Some(pe.display_name.clone());
        Ok(())
    }

    fn install_pe_boot_entry(&self) -> Result<(), InstallBackendError> {
        let path = self.pe_path.as_ref().ok_or_else(|| {
            InstallBackendError::new("pe_not_verified", "PE cache verification has not completed")
        })?;
        let display_name = self.pe_display_name.as_deref().ok_or_else(|| {
            InstallBackendError::new("pe_name_missing", "PE display name is unavailable")
        })?;
        super::pe::PeManager::new()
            .boot_to_pe(&path.to_string_lossy(), display_name)
            .map_err(|error| Self::error("install_pe_boot_entry", error))
    }

    fn select_data_partition(
        &mut self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let image_size = if intent.options.is_xp_i386 {
            let source = Path::new(&intent.image_path);
            let arch = lr_core::xp_i386::validate_i386_source(source)
                .map_err(|error| Self::error("invalid_xp_source", error))?;
            let mut size = Self::directory_size_checked(source)?;
            if arch == "AMD64" {
                let sibling = source
                    .parent()
                    .map(|parent| parent.join("I386"))
                    .filter(|path| path.is_dir());
                if let Some(sibling) = sibling {
                    size = size.saturating_add(Self::directory_size_checked(&sibling)?);
                }
            }
            size.saturating_add(64 * 1024 * 1024)
        } else {
            std::fs::metadata(&intent.image_path)
                .map_err(|error| Self::error("inspect_source_image", error))?
                .len()
        };
        let selected =
            DiskManager::find_suitable_data_partition(&intent.target_partition, image_size)
                .map_err(|error| Self::error("select_data_partition", error))?
                .ok_or_else(|| {
                    InstallBackendError::new(
                        "no_data_partition",
                        "no safe data partition has enough space for the source image",
                    )
                })?;
        self.data_partition = Some(selected.0);
        std::fs::create_dir_all(self.data_dir()?)
            .map_err(|error| Self::error("create_data_directory", error))
    }

    fn persist_pca_package(&self) -> Result<(), InstallBackendError> {
        let Some(package) = self.pca_package.as_ref() else {
            return Ok(());
        };
        package
            .persist_to(
                &Path::new(&self.data_dir()?)
                    .join(lr_core::pca_compat::STAGED_PACKAGE_RELATIVE_PATH),
            )
            .map_err(|error| Self::error("persist_pca_package", error))
    }

    fn verify_source_image(
        &self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        use super::image_verify::{ImageVerifier, VerifyStatus};

        if cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "source image verification was cancelled before it started",
            ));
        }

        if intent.options.is_xp_i386 {
            lr_core::xp_i386::validate_i386_source(Path::new(&intent.image_path))
                .map_err(|error| Self::error("invalid_xp_source", error))?;
            Self::report(
                reporter,
                InstallExecutionPhase::VerifySourceImage,
                100,
                crate::tr!("XP/2003 安装源校验通过"),
            );
            return Ok(());
        }

        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let image = intent.image_path.clone();
        let verify_cancel = Arc::new(AtomicBool::new(false));
        let verify_cancel_for_worker = Arc::clone(&verify_cancel);
        std::thread::spawn(move || {
            let result = ImageVerifier::with_cancel_flag(verify_cancel_for_worker)
                .verify(&image, Some(progress_tx));
            let _ = result_tx.send(result);
        });
        let mut cancellation_reported = false;
        let result = loop {
            while let Ok(progress) = progress_rx.try_recv() {
                Self::report(
                    reporter,
                    InstallExecutionPhase::VerifySourceImage,
                    progress.percentage,
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => break result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "source_verify_worker_disconnected",
                        "the source verification worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() && !cancellation_reported {
                cancellation_reported = true;
                verify_cancel.store(true, Ordering::SeqCst);
                Self::report(
                    reporter,
                    InstallExecutionPhase::VerifySourceImage,
                    0,
                    crate::tr!("已请求取消；校验将在安全点停止。"),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        if result.status == VerifyStatus::Cancelled || cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "source image verification was cancelled",
            ));
        }
        if result.status == VerifyStatus::Valid {
            Ok(())
        } else {
            Err(InstallBackendError::new(
                "source_image_verification_failed",
                format!("{}: {}", result.status, result.message),
            ))
        }
    }

    fn copy_source_image(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        if intent.options.is_xp_i386 {
            return self.copy_xp_source(intent, reporter, cancellation);
        }
        let file_name = Path::new(&intent.image_path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                InstallBackendError::new("invalid_image_name", "source image has no file name")
            })?
            .to_string();
        let destination = Path::new(&self.data_dir()?).join(&file_name);
        let source = std::fs::File::open(&intent.image_path)
            .map_err(|error| Self::error("open_source_image", error))?;
        let total = source
            .metadata()
            .map_err(|error| Self::error("inspect_source_image", error))?
            .len();
        let destination_file = std::fs::File::create(&destination)
            .map_err(|error| Self::error("create_staged_image", error))?;
        let mut reader = BufReader::with_capacity(1024 * 1024, source);
        let mut writer = BufWriter::with_capacity(1024 * 1024, destination_file);
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut copied = 0_u64;
        loop {
            if cancellation.is_cancelled() {
                drop(writer);
                let _ = std::fs::remove_file(&destination);
                return Err(InstallBackendError::new(
                    "cancelled",
                    "image copy was cancelled",
                ));
            }
            let count = reader
                .read(&mut buffer)
                .map_err(|error| Self::error("read_source_image", error))?;
            if count == 0 {
                break;
            }
            writer
                .write_all(&buffer[..count])
                .map_err(|error| Self::error("write_staged_image", error))?;
            copied += count as u64;
            let percentage = if total == 0 {
                100
            } else {
                ((copied.saturating_mul(100) / total).min(100)) as u8
            };
            Self::report(
                reporter,
                InstallExecutionPhase::CopySourceImage,
                percentage,
                file_name.clone(),
            );
        }
        writer
            .flush()
            .map_err(|error| Self::error("flush_staged_image", error))?;
        let staged_size = std::fs::metadata(&destination)
            .map_err(|error| Self::error("inspect_staged_image", error))?
            .len();
        if staged_size != total {
            let _ = std::fs::remove_file(&destination);
            return Err(InstallBackendError::new(
                "staged_image_size_mismatch",
                format!("expected {total} bytes, copied {staged_size} bytes"),
            ));
        }
        self.staged_image_name = Some(file_name);
        Ok(())
    }

    fn directory_size_checked(source: &Path) -> Result<u64, InstallBackendError> {
        let metadata = std::fs::symlink_metadata(source)
            .map_err(|error| Self::error("inspect_xp_source", error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(InstallBackendError::new(
                "unsafe_xp_source_entry",
                format!(
                    "XP source is not an ordinary directory: {}",
                    source.display()
                ),
            ));
        }
        let mut size = 0_u64;
        for entry in
            std::fs::read_dir(source).map_err(|error| Self::error("read_xp_source", error))?
        {
            let entry = entry.map_err(|error| Self::error("read_xp_source_entry", error))?;
            let metadata = entry
                .metadata()
                .map_err(|error| Self::error("inspect_xp_source_entry", error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| Self::error("inspect_xp_source_entry", error))?;
            if file_type.is_symlink() {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!("XP source contains a link: {}", entry.path().display()),
                ));
            }
            if file_type.is_dir() {
                size = size.saturating_add(Self::directory_size_checked(&entry.path())?);
            } else if file_type.is_file() {
                size = size.saturating_add(metadata.len());
            } else {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!(
                        "XP source contains a special entry: {}",
                        entry.path().display()
                    ),
                ));
            }
        }
        Ok(size)
    }

    fn copy_xp_tree(
        source: &Path,
        destination: &Path,
        total: u64,
        copied: &mut u64,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        std::fs::create_dir_all(destination)
            .map_err(|error| Self::error("create_staged_xp_directory", error))?;
        for entry in
            std::fs::read_dir(source).map_err(|error| Self::error("read_xp_source", error))?
        {
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "XP source copy was cancelled",
                ));
            }
            let entry = entry.map_err(|error| Self::error("read_xp_source_entry", error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| Self::error("inspect_xp_source_entry", error))?;
            let target = destination.join(entry.file_name());
            if file_type.is_symlink() {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!("XP source contains a link: {}", entry.path().display()),
                ));
            }
            if file_type.is_dir() {
                Self::copy_xp_tree(
                    &entry.path(),
                    &target,
                    total,
                    copied,
                    reporter,
                    cancellation,
                )?;
            } else if file_type.is_file() {
                let bytes = std::fs::copy(entry.path(), &target)
                    .map_err(|error| Self::error("copy_xp_source_file", error))?;
                *copied = copied.saturating_add(bytes);
                let percentage = if total == 0 {
                    100
                } else {
                    ((copied.saturating_mul(100) / total).min(100)) as u8
                };
                Self::report(
                    reporter,
                    InstallExecutionPhase::CopySourceImage,
                    percentage,
                    crate::tr!("正在暂存 XP/2003 安装源..."),
                );
            } else {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!(
                        "XP source contains a special entry: {}",
                        entry.path().display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn copy_xp_source(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let source = Path::new(&intent.image_path);
        let arch = lr_core::xp_i386::validate_i386_source(source)
            .map_err(|error| Self::error("invalid_xp_source", error))?;
        let sibling_i386 = (arch == "AMD64")
            .then(|| source.parent().map(|parent| parent.join("I386")))
            .flatten()
            .filter(|path| path.is_dir());
        let mut total = Self::directory_size_checked(source)?;
        if let Some(sibling) = sibling_i386.as_ref() {
            total = total.saturating_add(Self::directory_size_checked(sibling)?);
        }

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Self::error("stage_xp_clock", error))?
            .as_nanos();
        let root_name = format!("xp-source-{}-{nonce}", std::process::id());
        let data_dir = PathBuf::from(self.data_dir()?);
        let final_root = data_dir.join(&root_name);
        let temporary_root = data_dir.join(format!(".{root_name}.partial"));
        let mut copied = 0_u64;
        let result = (|| {
            Self::copy_xp_tree(
                source,
                &temporary_root.join(arch),
                total,
                &mut copied,
                reporter,
                cancellation,
            )?;
            if let Some(sibling) = sibling_i386.as_ref() {
                Self::copy_xp_tree(
                    sibling,
                    &temporary_root.join("I386"),
                    total,
                    &mut copied,
                    reporter,
                    cancellation,
                )?;
            }
            if copied != total {
                return Err(InstallBackendError::new(
                    "staged_xp_source_size_mismatch",
                    format!("expected {total} bytes, copied {copied} bytes"),
                ));
            }
            lr_core::xp_i386::validate_i386_source(&temporary_root.join(arch))
                .map_err(|error| Self::error("staged_xp_source_invalid", error))?;
            std::fs::rename(&temporary_root, &final_root)
                .map_err(|error| Self::error("commit_staged_xp_source", error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temporary_root);
        }
        result?;
        self.staged_image_name = Some(root_name);
        self.staged_xp_source_arch = Some(arch.to_string());
        Ok(())
    }

    fn stage_uefiseven(&self) -> Result<(), InstallBackendError> {
        let source = crate::utils::path::get_uefiseven_dir();
        let destination = Path::new(&self.data_dir()?).join("uefiseven");
        if !source.is_dir() {
            log::warn!(
                "[NATIVE INSTALL] UefiSeven source is missing: {}",
                source.display()
            );
            return Ok(());
        }
        std::fs::create_dir_all(&destination)
            .map_err(|error| Self::error("create_uefiseven_stage", error))?;
        for name in ["bootx64.efi", "UefiSeven.ini"] {
            let from = source.join(name);
            if from.is_file() {
                if let Err(error) = std::fs::copy(&from, destination.join(name)) {
                    log::warn!("[NATIVE INSTALL] failed to stage {name}: {error}");
                }
            }
        }
        Ok(())
    }

    fn directory_has_inf(directory: &Path) -> bool {
        let mut pending = vec![directory.to_path_buf()];
        while let Some(path) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
                {
                    return true;
                }
            }
        }
        false
    }

    fn stage_user_drivers(&self) -> Result<(), InstallBackendError> {
        let root = Path::new(&self.data_dir()?).join("user_drivers");
        for version in ["win7", "win8", "win10", "win11"] {
            let source = crate::utils::path::get_drivers_dir().join(version);
            if !Self::directory_has_inf(&source) {
                continue;
            }
            if let Err(error) = Self::copy_directory(&source, &root.join(version)) {
                log::warn!("[NATIVE INSTALL] staging user drivers {version} failed: {error}");
            }
        }
        Ok(())
    }

    fn write_pe_install_config(
        &self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let staged_name = self.staged_image_name.as_deref().ok_or_else(|| {
            InstallBackendError::new("staged_image_missing", "source image has not been staged")
        })?;
        let pca = self.pca_package.as_ref().map(|package| PcaCompatConfig {
            package: lr_core::pca_compat::STAGED_PACKAGE_RELATIVE_PATH.to_string(),
            sha256: package.sha256().to_string(),
            image_index: package.image_index(),
            target_build: package.target().build,
            target_architecture: package.target().architecture,
        });
        let mut config =
            intent.to_install_config(staged_name, lr_core::active_engine().as_u8(), pca.as_ref());
        if intent.options.is_xp_i386 {
            config.xp_source_arch = self.staged_xp_source_arch.clone().ok_or_else(|| {
                InstallBackendError::new(
                    "staged_xp_arch_missing",
                    "XP source architecture has not been staged",
                )
            })?;
        }
        super::install_config::ConfigFileManager::write_install_config(
            &intent.target_partition,
            self.data_partition()?,
            &config,
        )
        .map_err(|error| Self::error("write_pe_install_config", error))
    }

    fn refresh_target(
        &mut self,
        context: &InstallExecutionContext,
    ) -> Result<(), InstallBackendError> {
        let identity = context.stable_target.ok_or_else(|| {
            InstallBackendError::new("missing_stable_target", "stable target identity is absent")
        })?;
        let partitions = DiskManager::get_partitions()
            .map_err(|error| Self::error("enumerate_partitions", error))?;
        let target = partitions
            .iter()
            .find(|partition| {
                partition.disk_number == Some(identity.disk_number)
                    && partition.partition_number == Some(identity.partition_number)
            })
            .ok_or_else(|| {
                InstallBackendError::new(
                    "target_identity_changed",
                    format!(
                        "disk {} partition {} no longer exists or has no usable drive letter",
                        identity.disk_number, identity.partition_number
                    ),
                )
            })?;
        if target.letter.trim().is_empty() {
            return Err(InstallBackendError::new(
                "target_has_no_letter",
                "the verified target partition has no drive letter",
            ));
        }
        self.target.clone_from(&target.letter);
        self.target_style = target.partition_style;
        self.partitions = partitions;
        Ok(())
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn refresh_target_after_diskpart(
        &mut self,
        context: &InstallExecutionContext,
    ) -> Result<(), InstallBackendError> {
        let identity = context.stable_target.ok_or_else(|| {
            InstallBackendError::new("missing_stable_target", "stable target identity is absent")
        })?;
        for attempt in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(if attempt == 0 {
                800
            } else {
                500
            }));
            if self.refresh_target(context).is_ok() {
                return Ok(());
            }
        }

        let free = DiskManager::find_available_drive_letter().ok_or_else(|| {
            InstallBackendError::new(
                "no_free_drive_letter",
                "no free drive letter is available for the verified target partition",
            )
        })?;
        let script = format!(
            "select disk {}\r\nselect partition {}\r\nremove noerr\r\nassign letter={}\r\nexit\r\n",
            identity.disk_number, identity.partition_number, free
        );
        let output = lr_core::diskpart::execute_script(
            &std::env::temp_dir(),
            "lr-assign-target",
            "diskpart",
            &script,
        )
        .map_err(|error| Self::error("assign_target_letter", error))?;
        let output_text = lr_core::diskpart::validated_stdout(&output)
            .map_err(|error| Self::error("assign_target_letter", error))?;
        log::info!(
            "[NATIVE INSTALL] assigned drive {free}: to disk {} partition {}: {}",
            identity.disk_number,
            identity.partition_number,
            output_text.trim()
        );

        for attempt in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(if attempt == 0 {
                800
            } else {
                500
            }));
            if self.refresh_target(context).is_ok() {
                return Ok(());
            }
        }
        Err(InstallBackendError::new(
            "target_identity_changed",
            format!(
                "disk {} partition {} no longer exists or could not be assigned a drive letter; DiskPart output: {}",
                identity.disk_number,
                identity.partition_number,
                output_text.trim()
            ),
        ))
    }

    fn report(
        reporter: &mut dyn InstallExecutionReporter,
        phase: InstallExecutionPhase,
        percentage: u8,
        detail: impl Into<String>,
    ) {
        reporter.report(InstallExecutionEvent::Progress {
            phase,
            percentage,
            detail: detail.into(),
        });
    }

    fn apply_wim(
        &self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        if cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "WIM apply was cancelled before it started",
            ));
        }
        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let image = intent.image_path.clone();
        let target = format!("{}\\", self.target);
        let volume_index = intent.volume_index;
        let apply_cancel = Arc::new(AtomicBool::new(false));
        let apply_cancel_for_worker = Arc::clone(&apply_cancel);
        std::thread::spawn(move || {
            let result = super::dism::Dism::new().apply_image_cancellable(
                &image,
                &target,
                volume_index,
                Some(progress_tx),
                Some(apply_cancel_for_worker),
            );
            let _ = result_tx.send(result);
        });
        let mut cancellation_reported = false;
        loop {
            while let Ok(progress) = progress_rx.try_recv() {
                Self::report(
                    reporter,
                    InstallExecutionPhase::ApplyWimImage,
                    progress.percentage,
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => {
                    while let Ok(progress) = progress_rx.try_recv() {
                        Self::report(
                            reporter,
                            InstallExecutionPhase::ApplyWimImage,
                            progress.percentage,
                            progress.status,
                        );
                    }
                    if cancellation.is_cancelled() {
                        return Err(InstallBackendError::new(
                            "cancelled",
                            "WIM apply was cancelled",
                        ));
                    }
                    return result.map_err(|error| Self::error("apply_wim", error));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "apply_wim_worker_disconnected",
                        "the WIM apply worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() && !cancellation_reported {
                cancellation_reported = true;
                apply_cancel.store(true, Ordering::SeqCst);
                Self::report(
                    reporter,
                    InstallExecutionPhase::ApplyWimImage,
                    0,
                    crate::tr!("已请求取消；镜像引擎将在安全点停止。"),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn apply_ghost(
        &self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let ghost = super::ghost::Ghost::new();
        if !ghost.is_available() {
            return Err(InstallBackendError::new(
                "ghost_unavailable",
                "Ghost executable is unavailable",
            ));
        }
        let cancel_flag = ghost.get_cancel_flag();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let image = intent.image_path.clone();
        let target = self.target.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let result =
                ghost.restore_image_to_letter(&image, &target, &partitions, Some(progress_tx));
            let _ = result_tx.send(result);
        });
        loop {
            while let Ok(progress) = progress_rx.try_recv() {
                Self::report(
                    reporter,
                    InstallExecutionPhase::ApplyGhostImage,
                    progress.percentage,
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => {
                    return result.map_err(|error| Self::error("apply_ghost", error));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "apply_ghost_worker_disconnected",
                        "the Ghost worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() {
                cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn legacy_advanced(
        intent: &StartInstallIntent,
    ) -> super::advanced_options_legacy::AdvancedOptions {
        (&intent.options.advanced_options).into()
    }

    fn copy_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let destination = destination.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                Self::copy_directory(&entry.path(), &destination)?;
            } else {
                std::fs::copy(entry.path(), destination)?;
            }
        }
        Ok(())
    }

    fn process_drivers(&self, intent: &StartInstallIntent) {
        if !self.driver_backup.exists() {
            return;
        }
        let backup = self.driver_backup.to_string_lossy();
        match intent.options.driver_action {
            DriverAction::AutoImport => {
                if let Err(error) = super::dism::Dism::new()
                    .add_drivers_offline(&format!("{}\\", self.target), &backup)
                {
                    // Preserve the legacy best-effort policy: image deployment
                    // must not be reported as failed solely due to driver import.
                    log::error!("[NATIVE INSTALL] driver import failed: {error}");
                }
                let _ = std::fs::remove_dir_all(&self.driver_backup);
            }
            DriverAction::SaveOnly => {
                let destination = PathBuf::from(format!("{}\\LetRecovery_Drivers", self.target));
                if let Err(error) = Self::copy_directory(&self.driver_backup, &destination) {
                    log::error!("[NATIVE INSTALL] preserving driver backup failed: {error}");
                }
                let _ = std::fs::remove_dir_all(&self.driver_backup);
            }
            DriverAction::None => {}
        }
    }

    fn format_target_compat(&self, intent: &StartInstallIntent) -> Result<(), InstallBackendError> {
        let plan = Self::format_plan_for_intent(&self.target, intent)?;
        let output = lr_core::diskpart::execute_script(
            &std::env::temp_dir(),
            "lr-native-format",
            "diskpart",
            &plan.diskpart_script,
        )
        .map_err(|error| Self::error("start_diskpart_format", error))?;
        let stdout = crate::utils::encoding::gbk_to_utf8(output.stdout());
        let stderr = crate::utils::encoding::gbk_to_utf8(output.stderr());
        if native_install_compat::diskpart_format_succeeded(&stdout)
            && !lr_core::diskpart::output_indicates_error(output.succeeded(), &stdout, &stderr)
        {
            return Ok(());
        }

        log::warn!(
            "[NATIVE INSTALL] DiskPart format failed, using typed format.com fallback: {} {}",
            stdout.trim(),
            stderr.trim()
        );
        let fallback = SystemCommandExecutor
            .execute(&plan.fallback)
            .map_err(|error| Self::error("start_format_fallback", error))?;
        let fallback_stdout = crate::utils::encoding::gbk_to_utf8(fallback.stdout());
        let fallback_stderr = crate::utils::encoding::gbk_to_utf8(fallback.stderr());
        if native_install_compat::fallback_format_succeeded(
            fallback.succeeded(),
            &fallback_stdout,
            &fallback_stderr,
        ) {
            Ok(())
        } else {
            Err(InstallBackendError::new(
                "format_target",
                format!(
                    "DiskPart: {} {}; format.com: {} {}",
                    stdout.trim(),
                    stderr.trim(),
                    fallback_stdout.trim(),
                    fallback_stderr.trim()
                ),
            ))
        }
    }

    fn format_plan_for_intent(
        target: &str,
        intent: &StartInstallIntent,
    ) -> Result<native_install_compat::FormatCompatibilityPlan, InstallBackendError> {
        let advanced = &intent.options.advanced_options;
        let label = (advanced.custom_volume_label && !advanced.volume_label.trim().is_empty())
            .then_some(advanced.volume_label.as_str());
        native_install_compat::build_format_plan(target, label)
            .map_err(|error| Self::error("invalid_format_plan", error))
    }

    fn deactivate_xp_sibling_partitions(&self) {
        let identities = self
            .partitions
            .iter()
            .map(|partition| PartitionIdentity {
                letter: partition.letter.as_str(),
                disk_number: partition.disk_number,
            })
            .collect::<Vec<_>>();
        for (letter, script) in
            native_install_compat::sibling_inactive_scripts(&self.target, &identities)
        {
            if let Err(error) = lr_core::diskpart::execute_script_checked(
                &std::env::temp_dir(),
                "lr-xp-deactivate",
                "diskpart",
                &script,
            ) {
                // Preserve the old best-effort cleanup policy. The XP engine's
                // own target activation remains authoritative.
                log::warn!(
                    "[NATIVE INSTALL] failed to clear active flag on sibling {letter}: {error}"
                );
            }
        }
    }

    fn ensure_mbr_signature(&self, disk_number: u32) -> Result<(), InstallBackendError> {
        let output = lr_core::diskpart::execute_script(
            &std::env::temp_dir(),
            "lr-signature-read",
            "diskpart",
            &native_install_compat::mbr_signature_read_script(disk_number),
        )
        .map_err(|error| Self::error("read_mbr_signature", error))?;
        let stdout = crate::utils::encoding::gbk_to_utf8(output.stdout());
        match native_install_compat::parse_mbr_signature(&stdout) {
            MbrSignatureObservation::NonZero(signature) => {
                log::info!("[NATIVE INSTALL] disk {disk_number} keeps MBR signature {signature}");
                Ok(())
            }
            MbrSignatureObservation::NotMbrOrUnparseable => {
                log::warn!(
                    "[NATIVE INSTALL] disk {disk_number} has no unambiguous MBR signature; skipped"
                );
                Ok(())
            }
            MbrSignatureObservation::Zero => {
                let entropy = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.subsec_nanos() ^ duration.as_secs() as u32)
                    .unwrap_or(0xA1B2_C3D4);
                let signature = native_install_compat::replacement_mbr_signature(entropy);
                let script =
                    native_install_compat::mbr_signature_write_script(disk_number, signature)
                        .ok_or_else(|| {
                            InstallBackendError::new(
                                "invalid_mbr_signature",
                                "replacement ID was zero",
                            )
                        })?;
                lr_core::diskpart::execute_script_checked(
                    &std::env::temp_dir(),
                    "lr-signature-write",
                    "diskpart",
                    &script,
                )
                .map(|_| ())
                .map_err(|error| Self::error("write_mbr_signature", error))
            }
        }
    }

    fn inject_versioned_user_drivers(&self, is_xp: bool) {
        if is_xp {
            return;
        }
        let ntdll = Path::new(&self.target)
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        let Some((major, minor, build, _)) = super::system_utils::get_file_version(&ntdll) else {
            log::warn!("[NATIVE INSTALL] cannot identify target version for user drivers");
            return;
        };
        let family = native_install_compat::classify_windows_version(major, minor, build);
        let Some(source) = native_install_compat::user_driver_source(
            &crate::utils::path::get_drivers_dir(),
            family,
        ) else {
            return;
        };
        if !Self::directory_has_inf(&source) {
            return;
        }
        if let Err(error) = super::dism::Dism::new()
            .add_drivers_offline(&format!("{}\\", self.target), &source.to_string_lossy())
        {
            // This remains best effort exactly as in the old progress worker.
            log::warn!("[NATIVE INSTALL] versioned user driver injection failed: {error}");
        }
    }

    fn write_unattend(&self, intent: &StartInstallIntent) -> Result<(), InstallBackendError> {
        let panther = Path::new(&self.target).join("Windows").join("Panther");
        std::fs::create_dir_all(&panther).map_err(|error| Self::error("create_panther", error))?;
        let destination = panther.join("unattend.xml");
        if !intent.options.custom_unattend_path.trim().is_empty() {
            std::fs::copy(&intent.options.custom_unattend_path, &destination)
                .map_err(|error| Self::error("copy_custom_unattend", error))?;
            return Ok(());
        }

        let architecture = match super::system_utils::get_system_architecture(&self.target) {
            super::system_utils::SystemArchitecture::X86 => UnattendArchitecture::X86,
            super::system_utils::SystemArchitecture::Amd64 => UnattendArchitecture::Amd64,
            unexpected => {
                return Err(InstallBackendError::new(
                    "unsupported_unattend_architecture",
                    format!("unsupported target architecture: {unexpected:?}"),
                ));
            }
        };
        let ntdll = Path::new(&self.target)
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        let family = super::system_utils::get_file_version(&ntdll)
            .map(|(major, minor, build, _)| {
                native_install_compat::classify_windows_version(major, minor, build)
            })
            .unwrap_or(native_install_compat::WindowsFamily::Unsupported);
        let advanced = &intent.options.advanced_options;
        let xml = native_install_compat::render_default_unattend(&DefaultUnattendOptions {
            architecture,
            family,
            username: advanced
                .custom_username
                .then_some(advanced.username.as_str()),
            remove_uwp_apps: advanced.remove_uwp_apps,
        });
        std::fs::write(&destination, &xml)
            .map_err(|error| Self::error("write_default_unattend", error))?;
        let sysprep = Path::new(&self.target)
            .join("Windows")
            .join("System32")
            .join("Sysprep");
        if sysprep.is_dir() {
            if let Err(error) = std::fs::write(sysprep.join("unattend.xml"), xml) {
                log::warn!("[NATIVE INSTALL] writing Sysprep unattend failed: {error}");
            }
        }
        Ok(())
    }

    fn repair_boot(&mut self, intent: &StartInstallIntent) -> Result<(), InstallBackendError> {
        if let Some(package) = self.pca_package.as_ref() {
            package
                .inject_into_offline_windows(Path::new(&format!("{}\\", self.target)))
                .map_err(|error| Self::error("inject_pca2023", error))?;
        }

        let is_xp =
            intent.options.is_xp || !Path::new(&format!("{}\\Windows\\Boot", self.target)).exists();
        let use_uefi = match intent.options.boot_mode {
            BootModeSelection::UEFI => true,
            BootModeSelection::Legacy => false,
            BootModeSelection::Auto => self.target_style == PartitionStyle::GPT,
        };
        let manager = super::bcdedit::BootManager::new();
        if !use_uefi {
            if let Some(disk_number) = self
                .partitions
                .iter()
                .find(|partition| partition.letter.eq_ignore_ascii_case(&self.target))
                .and_then(|partition| partition.disk_number)
            {
                // Legacy behavior is best effort: an unreadable signature is
                // logged, while a proven zero signature is repaired.
                if let Err(error) = self.ensure_mbr_signature(disk_number) {
                    log::warn!("[NATIVE INSTALL] MBR signature check failed: {error:?}");
                }
            }
        }
        if is_xp {
            if use_uefi {
                if let Err(primary) = manager.write_xp_uefi_gpt_boot(&self.target) {
                    log::warn!(
                        "[NATIVE INSTALL] XP UEFI boot failed ({primary}); falling back to NTLDR"
                    );
                    manager
                        .write_xp_boot(&self.target)
                        .map_err(|error| Self::error("repair_xp_boot", error))?;
                }
            } else {
                manager
                    .write_xp_boot(&self.target)
                    .map_err(|error| Self::error("repair_xp_boot", error))?;
            }
        } else {
            manager
                .repair_boot_advanced(&self.target, use_uefi, intent.options.boot_pca_mode)
                .map_err(|error| Self::error("repair_boot", error))?;
        }

        if use_uefi && intent.options.advanced_options.win7_uefi_patch {
            let advanced = Self::legacy_advanced(intent);
            if let Err(error) = advanced.apply_uefiseven_patch(&self.target) {
                log::warn!("[NATIVE INSTALL] UefiSeven patch failed; continuing: {error}");
            }
        }
        Ok(())
    }
}

impl InstallExecutionBackend for ProductionInstallBackend {
    fn execute_phase(
        &mut self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        phase: InstallExecutionPhase,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = (intent, context, phase, reporter, cancellation);
            Err(InstallBackendError::new(
                "development_build_denied",
                "production install backend is disabled in non-elevated development builds",
            ))
        }

        #[cfg(not(feature = "non-elevated-tests"))]
        {
            let supported = match intent.mode {
                InstallMode::Direct => Self::supports_direct_phase(phase),
                InstallMode::ViaPe => Self::supports_via_pe_phase(phase),
            };
            if !supported {
                return Err(InstallBackendError::new(
                    UNSUPPORTED_PENDING,
                    format!("phase {phase:?} does not belong to the selected install mode"),
                ));
            }
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "installation cancelled",
                ));
            }
            match phase {
                InstallExecutionPhase::InspectBitLocker => {
                    self.inspect_bitlocker_fresh(intent, context, reporter, cancellation)
                }
                InstallExecutionPhase::AwaitBitLockerDecryption => {
                    self.await_bitlocker_fallback_decryption(reporter, cancellation)
                }
                InstallExecutionPhase::VerifyPcaBeforeDiskWrite => {
                    let may_use_uefi = intent.options.boot_mode != BootModeSelection::Legacy;
                    self.pca_package = super::pca_preflight::verify_before_disk_write(
                        &intent.image_path,
                        intent.volume_index,
                        intent.is_gho,
                        intent.options.is_xp || intent.options.is_xp_i386,
                        may_use_uefi,
                        intent.options.boot_pca_mode,
                    )
                    .map_err(|error| Self::error("pca_preflight", error))?;
                    Ok(())
                }
                InstallExecutionPhase::ResolveStableTarget => self.refresh_target(context),
                InstallExecutionPhase::ResolveTargetAfterDiskpart => {
                    self.refresh_target_after_diskpart(context)
                }
                InstallExecutionPhase::RunDiskpartScripts => {
                    let directory = crate::utils::path::get_diskpart_scripts_dir();
                    // Legacy semantics intentionally continue after an individual
                    // script error, then fail closed if stable target resolution fails.
                    if let Err(error) = lr_core::diskpart::run_scripts_in_dir(&directory) {
                        log::warn!("[NATIVE INSTALL] DiskPart scripts reported errors: {error}");
                    }
                    Ok(())
                }
                InstallExecutionPhase::FormatTarget => {
                    if !intent.options.format_partition {
                        return Ok(());
                    }
                    self.format_target_compat(intent)
                }
                InstallExecutionPhase::ExportHostDrivers => {
                    if self.driver_backup.exists() {
                        std::fs::remove_dir_all(&self.driver_backup)
                            .map_err(|error| Self::error("clear_driver_backup", error))?;
                    }
                    // Driver preservation is optional.  The legacy install
                    // worker warned and continued when export failed so an
                    // otherwise valid image deployment was never blocked by
                    // a host-driver enumeration problem.
                    if let Err(error) = super::dism::Dism::new()
                        .export_drivers(&self.driver_backup.to_string_lossy())
                    {
                        log::warn!(
                            "[NATIVE INSTALL] host driver export failed; continuing without the optional backup: {error}"
                        );
                    }
                    Ok(())
                }
                InstallExecutionPhase::ApplyXpTextModeSource => {
                    self.deactivate_xp_sibling_partitions();
                    let custom = (!intent.options.custom_unattend_path.trim().is_empty())
                        .then(|| Path::new(&intent.options.custom_unattend_path));
                    lr_core::xp_i386::install_from_i386(
                        Path::new(&intent.image_path),
                        &self.target,
                        &crate::utils::path::get_bin_dir(),
                        custom,
                    )
                    .map(|_| ())
                    .map_err(|error| Self::error("apply_xp_i386", error))
                }
                InstallExecutionPhase::ApplyGhostImage => {
                    self.apply_ghost(intent, reporter, cancellation)
                }
                InstallExecutionPhase::ApplyWimImage => {
                    self.apply_wim(intent, reporter, cancellation)
                }
                InstallExecutionPhase::ProcessDrivers => {
                    self.process_drivers(intent);
                    Ok(())
                }
                InstallExecutionPhase::RepairBoot => self.repair_boot(intent),
                InstallExecutionPhase::ApplyAdvancedOptions => {
                    let advanced = Self::legacy_advanced(intent);
                    let is_xp = intent.options.is_xp
                        || !Path::new(&format!("{}\\Windows\\Boot", self.target)).exists();
                    if let Err(error) = advanced.apply_to_system(&self.target, is_xp) {
                        if intent.options.advanced_options.disable_windows_defender {
                            return Err(Self::error("remove_defender_antivirus_engine", error));
                        }
                        // Preserve legacy best-effort semantics for other post-deployment tweaks.
                        log::error!("[NATIVE INSTALL] advanced options failed: {error}");
                    }
                    self.inject_versioned_user_drivers(is_xp);
                    if intent.options.unattended_install {
                        if let Err(error) = self.write_unattend(intent) {
                            log::warn!(
                                "[NATIVE INSTALL] unattended setup preparation failed; continuing for legacy compatibility: {error:?}"
                            );
                        }
                    }
                    Ok(())
                }
                InstallExecutionPhase::FinishDirectInstall => Ok(()),
                InstallExecutionPhase::VerifyPeEnvironment => self.verify_pe_environment(intent),
                InstallExecutionPhase::InstallPeBootEntry => self.install_pe_boot_entry(),
                InstallExecutionPhase::SelectDataPartition => self.select_data_partition(intent),
                InstallExecutionPhase::PersistPcaCompatibilityPackage => self.persist_pca_package(),
                InstallExecutionPhase::ExportDriversToPeData => {
                    let destination = Path::new(&self.data_dir()?).join("drivers");
                    // Keep the old Via-PE policy as well: exported host
                    // drivers improve compatibility but are not required to
                    // stage and execute the selected system image.
                    if let Err(error) =
                        super::dism::Dism::new().export_drivers(&destination.to_string_lossy())
                    {
                        log::warn!(
                            "[NATIVE INSTALL] PE driver export failed; continuing without the optional backup: {error}"
                        );
                    }
                    Ok(())
                }
                InstallExecutionPhase::VerifySourceImage => {
                    self.verify_source_image(intent, reporter, cancellation)
                }
                InstallExecutionPhase::CopySourceImage => {
                    self.copy_source_image(intent, reporter, cancellation)
                }
                InstallExecutionPhase::StageUefiSeven => self.stage_uefiseven(),
                InstallExecutionPhase::StageUserDrivers => self.stage_user_drivers(),
                InstallExecutionPhase::WritePeInstallConfig => self.write_pe_install_config(intent),
                // Deliberately does not call shutdown/reboot. The UI owns the
                // explicit user confirmation after ReadyToReboot is reported.
                InstallExecutionPhase::ReadyToRebootIntoPe => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::native_install_controller::{InstallOptions, StartInstallIntent};
    use crate::core::ui_state::AdvancedOptionsData;
    use lr_core::boot_pca::BootPcaMode;

    fn intent(mode: InstallMode) -> StartInstallIntent {
        StartInstallIntent {
            mode,
            target_partition: "E:".into(),
            target_disk_number: 1,
            target_partition_number: 2,
            image_path: "D:\\install.wim".into(),
            volume_index: 1,
            is_system_partition: false,
            selected_pe: None,
            is_gho: false,
            options: InstallOptions {
                format_partition: false,
                repair_boot: false,
                unattended_install: false,
                export_drivers: false,
                auto_reboot: false,
                boot_mode: BootModeSelection::Auto,
                boot_pca_mode: BootPcaMode::Auto,
                advanced_options: AdvancedOptionsData::default(),
                driver_action: DriverAction::None,
                custom_unattend_path: String::new(),
                is_xp: false,
                is_xp_i386: false,
                run_diskpart_scripts: false,
            },
        }
    }

    #[test]
    fn advanced_state_round_trips_to_established_business_type() {
        let mut value = intent(InstallMode::Direct);
        value.options.advanced_options.disable_uac = true;
        value.options.advanced_options.username = "LetRecovery".into();
        value.options.advanced_options.migrate_wifi = true;
        value.options.advanced_options.wifi_ssid = "Test Wi-Fi".into();
        value.options.advanced_options.wifi_profile_xml = "<WLANProfile />".into();
        let converted = ProductionInstallBackend::legacy_advanced(&value);
        assert!(converted.disable_uac);
        assert_eq!(converted.username, "LetRecovery");
        assert!(converted.migrate_wifi);
        assert_eq!(converted.wifi_ssid, "Test Wi-Fi");
        assert_eq!(converted.wifi_profile_xml, "<WLANProfile />");
    }

    #[test]
    fn via_pe_plan_is_fully_dispatched_and_never_contains_reboot_io() {
        use crate::core::native_install_executor::NativeInstallExecutor;

        let plan = NativeInstallExecutor::build_plan(
            &intent(InstallMode::ViaPe),
            &InstallExecutionContext::default(),
        )
        .expect("ViaPE plan");
        assert!(plan
            .iter()
            .copied()
            .all(ProductionInstallBackend::supports_via_pe_phase));
        assert_eq!(
            plan.last(),
            Some(&InstallExecutionPhase::ReadyToRebootIntoPe)
        );
        assert!(!plan.contains(&InstallExecutionPhase::FinishDirectInstall));
    }

    #[test]
    fn missing_pe_is_returned_as_download_preparation_boundary() {
        let error = ProductionInstallBackend::require_cached_pe(
            CachedArtifactStatus::Missing,
            "LetRecovery_PE.wim",
        )
        .expect_err("missing PE must not be accepted");
        assert_eq!(error.code, "pe_download_required");
        assert!(error.detail.contains("LetRecovery_PE.wim"));
    }

    #[test]
    fn every_direct_executor_phase_has_a_production_dispatch_branch() {
        use crate::core::native_install_executor::{
            BitLockerRequirement, NativeInstallExecutor, StableTargetIdentity,
        };

        let context = InstallExecutionContext {
            stable_target: Some(StableTargetIdentity {
                disk_number: 2,
                partition_number: 3,
            }),
            bitlocker: BitLockerRequirement::Ready,
        };
        let plan = NativeInstallExecutor::build_plan(&intent(InstallMode::Direct), &context)
            .expect("direct plan");
        assert!(plan
            .into_iter()
            .all(ProductionInstallBackend::supports_direct_phase));
        assert!(!ProductionInstallBackend::supports_direct_phase(
            InstallExecutionPhase::InstallPeBootEntry
        ));
    }

    #[test]
    fn direct_format_stage_uses_custom_label_in_both_compat_attempts() {
        let mut value = intent(InstallMode::Direct);
        value.options.format_partition = true;
        value.options.advanced_options.custom_volume_label = true;
        value.options.advanced_options.volume_label = "Windows 11".into();
        let plan = ProductionInstallBackend::format_plan_for_intent("E:", &value).unwrap();
        assert!(plan
            .diskpart_script
            .contains("label=\"Windows 11\" quick override"));
        assert!(plan
            .fallback
            .arguments()
            .iter()
            .any(|argument| argument == std::ffi::OsStr::new("/V:Windows 11")));
        assert!(plan
            .fallback
            .arguments()
            .iter()
            .any(|argument| argument == std::ffi::OsStr::new("/X")));
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_backend_refuses_before_any_io() {
        let intent = intent(InstallMode::Direct);
        let mut backend = ProductionInstallBackend::new(&intent);
        let mut reporter = |_: InstallExecutionEvent| {};
        let cancelled = || false;
        let error = backend
            .execute_phase(
                &intent,
                &InstallExecutionContext::default(),
                InstallExecutionPhase::FormatTarget,
                &mut reporter,
                &cancelled,
            )
            .unwrap_err();
        assert_eq!(error.code, "development_build_denied");
    }
}
