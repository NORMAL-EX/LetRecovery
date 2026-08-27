//! Worker boundary for backup launch intents emitted by the native UI.
//!
//! The native window owns presentation state; this module owns background execution and reports
//! typed messages.  It deliberately keeps reboot/shutdown outside this boundary.  A PE handoff
//! snapshots the selected local artifact, installs its boot entry and writes the existing
//! backward-compatible backup configuration.

#[cfg(any(not(feature = "non-elevated-tests"), test))]
use std::path::Path;
#[cfg(not(feature = "non-elevated-tests"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

#[cfg(any(not(feature = "non-elevated-tests"), test))]
use crate::core::native_backup_controller::DirectBackupTaskKind;
use crate::core::native_backup_controller::{BackupLaunchIntent, BackupLaunchMode};

#[cfg(not(feature = "non-elevated-tests"))]
use crate::core::dism::{Dism, DismProgress};
#[cfg(not(feature = "non-elevated-tests"))]
use crate::core::ghost::Ghost;
#[cfg(not(feature = "non-elevated-tests"))]
use crate::core::install_config::ConfigFileManager;
#[cfg(not(feature = "non-elevated-tests"))]
use crate::core::native_backup_controller::{DirectBackupIntent, PeBackupPreparationIntent};
#[cfg(not(feature = "non-elevated-tests"))]
use std::sync::mpsc::{self, Sender};

#[cfg(any(not(feature = "non-elevated-tests"), test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectOutputMetadata {
    is_regular_file: bool,
    length: u64,
}

#[cfg(any(not(feature = "non-elevated-tests"), test))]
trait DirectOutputMetadataReader {
    fn read_metadata(&self, path: &Path) -> std::io::Result<DirectOutputMetadata>;
}

#[cfg(not(feature = "non-elevated-tests"))]
struct SystemDirectOutputMetadataReader;

#[cfg(not(feature = "non-elevated-tests"))]
impl DirectOutputMetadataReader for SystemDirectOutputMetadataReader {
    fn read_metadata(&self, path: &Path) -> std::io::Result<DirectOutputMetadata> {
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(DirectOutputMetadata {
            is_regular_file: metadata.file_type().is_file(),
            length: metadata.len(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupWorkerMessage {
    Started {
        mode: BackupLaunchMode,
    },
    Progress {
        percentage: u8,
        status: String,
    },
    CancellationRequested {
        operation_may_still_be_running: bool,
    },
    PeCommitStarted,
    Completed {
        mode: BackupLaunchMode,
    },
    Cancelled {
        output_may_exist: bool,
    },
    Failed {
        mode: BackupLaunchMode,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupExecutionError {
    #[error("开发测试构建禁止执行真实备份或 PE 启动准备")]
    DisabledInDevelopment,
    #[error("无法启动备份工作线程: {0}")]
    Spawn(String),
    #[error("backup authorization failed: {0}")]
    UnsafeIntent(String),
}

#[cfg(not(feature = "non-elevated-tests"))]
#[derive(Debug, Clone)]
enum AuthorizedBackupLaunchIntent {
    Direct(AuthorizedDirectBackupIntent),
    ViaPe(AuthorizedPeBackupIntent),
}

#[cfg(not(feature = "non-elevated-tests"))]
#[derive(Debug, Clone)]
struct AuthorizedDirectBackupIntent {
    intent: DirectBackupIntent,
    authorization: crate::core::native_backup_controller::DirectBackupStableAuthorization,
}

#[cfg(not(feature = "non-elevated-tests"))]
#[derive(Debug, Clone)]
struct AuthorizedPeBackupIntent {
    intent: PeBackupPreparationIntent,
    authorization: crate::core::native_backup_controller::DirectBackupStableAuthorization,
}

/// Receiver and cooperative cancellation handle owned by the native UI.
///
/// WIM/ESD/SWM capture has no safe interrupt API in the existing engine.  For those formats a
/// cancellation request detaches progress but the worker reports that the operation may continue;
/// Ghost receives its existing cancellation flag and can terminate its child process.
pub struct BackupExecution {
    pub messages: Receiver<BackupWorkerMessage>,
    cancel_requested: Arc<AtomicBool>,
    ghost_cancel: Option<Arc<AtomicBool>>,
}

impl BackupExecution {
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
        if let Some(flag) = &self.ghost_cancel {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn cancellation_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn authorize_backup_intent(
    intent: BackupLaunchIntent,
) -> Result<AuthorizedBackupLaunchIntent, String> {
    use crate::core::bitlocker::VolumeStatus;
    use crate::core::native_backup_controller::DirectBackupStableAuthorization;
    use lr_core::backup_handoff::{BackupHandoffV2, BackupOutputPolicy};
    use lr_core::install_handoff::CanonicalInstallTargetV2;

    let (mut config, is_via_pe) = match &intent {
        BackupLaunchIntent::Direct(intent) => (intent.config.clone(), false),
        BackupLaunchIntent::ViaPe(intent) => (intent.config.clone(), true),
    };
    let source_letter = lr_core::windows_storage::path_drive_letter(Path::new(&format!(
        "{}\\",
        config.source_partition.trim_end_matches(['\\', '/'])
    )))
    .ok_or_else(|| "backup source must be a local drive root".to_owned())?;
    let destination_path = PathBuf::from(&config.save_path);
    let destination_letter = lr_core::windows_storage::path_drive_letter(&destination_path)
        .ok_or_else(|| "backup destination must be an absolute local drive path".to_owned())?;
    if !destination_path.is_absolute() {
        return Err("backup destination must be absolute".to_owned());
    }
    let destination_root = PathBuf::from(format!("{}:\\", destination_letter));
    let destination_relative_path = destination_path
        .strip_prefix(&destination_root)
        .map_err(|_| "backup destination is not rooted on its parsed local volume".to_owned())?
        .to_path_buf();
    lr_core::backup_handoff::validate_relative_output_path(&destination_relative_path)
        .map_err(|error| error.to_string())?;

    let source = lr_core::windows_storage::stable_volume_identity(source_letter)
        .map_err(|error| format!("bind backup source identity: {error}"))?;
    let destination = lr_core::windows_storage::stable_volume_identity(destination_letter)
        .map_err(|error| format!("bind backup destination identity: {error}"))?;
    if lr_core::windows_storage::same_stable_volume_identity(source, destination) {
        return Err("backup source and destination are the same stable volume".to_owned());
    }

    let source_status = strict_bitlocker_status(&format!("{}:", source_letter))?;
    let destination_status = strict_bitlocker_status(&format!("{}:", destination_letter))?;
    let allowed_direct = |status| {
        matches!(
            status,
            VolumeStatus::NotEncrypted | VolumeStatus::EncryptedUnlocked
        )
    };
    if is_via_pe {
        if source_status != VolumeStatus::NotEncrypted
            || destination_status != VolumeStatus::NotEncrypted
        {
            return Err(format!(
                "ViaPE backup requires unencrypted source and destination volumes (source={source_status:?}, destination={destination_status:?}); recovery secrets are never handed off"
            ));
        }
    } else if !allowed_direct(source_status) || !allowed_direct(destination_status) {
        return Err(format!(
            "Direct backup requires unencrypted or stably unlocked volumes (source={source_status:?}, destination={destination_status:?})"
        ));
    }

    let source_layout = lr_core::windows_storage::disk_layout_snapshot(source.extent.disk_number)
        .map_err(|error| format!("snapshot backup source disk: {error}"))?;
    let destination_layout =
        lr_core::windows_storage::disk_layout_snapshot(destination.extent.disk_number)
            .map_err(|error| format!("snapshot backup destination disk: {error}"))?;
    let source_canonical = CanonicalInstallTargetV2::from_snapshot(
        &source_layout,
        source.extent.offset_bytes,
        source.extent.extent_length_bytes,
    )
    .map_err(|error| format!("canonicalize backup source: {error}"))?;
    let destination_canonical = CanonicalInstallTargetV2::from_snapshot(
        &destination_layout,
        destination.extent.offset_bytes,
        destination.extent.extent_length_bytes,
    )
    .map_err(|error| format!("canonicalize backup destination: {error}"))?;

    let destination_metadata = std::fs::symlink_metadata(&destination_path);
    let destination_exists = match destination_metadata {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            true
        }
        Ok(_) => return Err("backup destination exists but is not a plain regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("inspect backup destination: {error}")),
    };
    let (output_policy, base_file) = match (destination_exists, config.incremental) {
        (false, true) => return Err(
            "incremental backup requires an existing WIM/ESD destination; refusing implicit create"
                .to_owned(),
        ),
        (true, incremental) => {
            if !matches!(config.format, 0 | 1) {
                return Err(
                    "replace/append publication is supported only for WIM/ESD backups".to_owned(),
                );
            }
            let base = lr_core::backup_atomic_publish::inspect_existing_file(&destination_path)
                .map_err(|error| format!("bind existing backup base identity: {error}"))?;
            (
                if incremental {
                    BackupOutputPolicy::Append
                } else {
                    BackupOutputPolicy::Replace
                },
                Some(lr_core::backup_handoff::BackupBaseFileIdentity {
                    length_bytes: base.length,
                    sha256: base.sha256,
                }),
            )
        }
        (false, false) => (BackupOutputPolicy::Create, None),
    };
    let handoff = BackupHandoffV2 {
        session_id: ConfigFileManager::new_session_id()
            .map_err(|error| format!("generate backup session identifier: {error}"))?,
        source: source_canonical,
        destination: destination_canonical,
        destination_relative_path,
        output_policy,
        base_file,
    };
    handoff.validate().map_err(|error| error.to_string())?;
    config.handoff = Some(handoff);
    let authorization = DirectBackupStableAuthorization {
        source,
        destination,
    };

    Ok(match intent {
        BackupLaunchIntent::Direct(mut intent) => {
            intent.config = config;
            AuthorizedBackupLaunchIntent::Direct(AuthorizedDirectBackupIntent {
                intent,
                authorization,
            })
        }
        BackupLaunchIntent::ViaPe(mut intent) => {
            intent.config = config;
            AuthorizedBackupLaunchIntent::ViaPe(AuthorizedPeBackupIntent {
                intent,
                authorization,
            })
        }
    })
}

#[cfg(not(feature = "non-elevated-tests"))]
fn strict_bitlocker_status(path: &str) -> Result<crate::core::bitlocker::VolumeStatus, String> {
    use crate::core::bitlocker::VolumeStatus;
    use lr_core::fveapi::FveError;

    let api = lr_core::fveapi::FveApi::instance()
        .map_err(|error| format!("load documented BitLocker status boundary: {error}"))?;
    match api.get_status_by_path(path) {
        Ok(info) => Ok(VolumeStatus::from(&info)),
        Err(FveError::NotEncrypted) | Err(FveError::NotBitLockerVolume) => {
            Ok(VolumeStatus::NotEncrypted)
        }
        Err(error) => Err(format!(
            "BitLocker status is not provably safe for {path}: {error}"
        )),
    }
}

/// Starts an existing, fully planned backup intent on a background thread.
///
/// The test-only non-elevated feature fails before a thread is created, so unit/UI development can
/// never capture an image, modify BCD, write a handoff marker or reboot the host.
#[cfg(feature = "non-elevated-tests")]
pub fn execute_backup(
    _intent: BackupLaunchIntent,
) -> Result<BackupExecution, BackupExecutionError> {
    Err(BackupExecutionError::DisabledInDevelopment)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn execute_backup(intent: BackupLaunchIntent) -> Result<BackupExecution, BackupExecutionError> {
    let intent = authorize_backup_intent(intent).map_err(BackupExecutionError::UnsafeIntent)?;
    let (messages_tx, messages) = mpsc::channel();
    let cancel_requested = Arc::new(AtomicBool::new(false));
    let cancel_for_worker = Arc::clone(&cancel_requested);

    // Create Ghost before moving into the worker so the UI handle can share its real cancel flag.
    let ghost = matches!(
        intent,
        AuthorizedBackupLaunchIntent::Direct(AuthorizedDirectBackupIntent {
            intent: DirectBackupIntent {
                task: DirectBackupTaskKind::Ghost,
                ..
            },
            ..
        })
    )
    .then(Ghost::new);
    let ghost_cancel = ghost.as_ref().map(Ghost::get_cancel_flag);

    std::thread::Builder::new()
        .name("letrecovery-native-backup".to_owned())
        .spawn(move || run_intent(intent, ghost, cancel_for_worker, messages_tx))
        .map_err(|error| BackupExecutionError::Spawn(error.to_string()))?;

    Ok(BackupExecution {
        messages,
        cancel_requested,
        ghost_cancel,
    })
}

#[cfg(not(feature = "non-elevated-tests"))]
fn run_intent(
    intent: AuthorizedBackupLaunchIntent,
    ghost: Option<Ghost>,
    cancel_requested: Arc<AtomicBool>,
    messages: Sender<BackupWorkerMessage>,
) {
    let mode = match &intent {
        AuthorizedBackupLaunchIntent::Direct(_) => BackupLaunchMode::Direct,
        AuthorizedBackupLaunchIntent::ViaPe(_) => BackupLaunchMode::ViaPe,
    };
    let _ = messages.send(BackupWorkerMessage::Started { mode });

    if cancel_requested.load(Ordering::SeqCst) {
        let _ = messages.send(BackupWorkerMessage::Cancelled {
            output_may_exist: false,
        });
        return;
    }

    let result: Result<(), BackupRunError> = match intent {
        AuthorizedBackupLaunchIntent::Direct(intent) => {
            match run_direct(intent, ghost, Arc::clone(&cancel_requested), &messages) {
                Ok(()) => Ok(()),
                Err(_error) if direct_failure_is_cancelled(true, &cancel_requested) => {
                    Err(BackupRunError::Cancelled {
                        output_may_exist: true,
                    })
                }
                Err(error) => Err(BackupRunError::Failed(error)),
            }
        }
        AuthorizedBackupLaunchIntent::ViaPe(intent) => {
            run_pe_handoff(intent, &cancel_requested, &messages)
        }
    };

    match result {
        Ok(()) if mode == BackupLaunchMode::Direct && cancel_requested.load(Ordering::SeqCst) => {
            let _ = messages.send(BackupWorkerMessage::Cancelled {
                output_may_exist: true,
            });
        }
        Ok(()) => {
            let _ = messages.send(BackupWorkerMessage::Completed { mode });
        }
        Err(BackupRunError::Cancelled { output_may_exist }) => {
            let _ = messages.send(BackupWorkerMessage::Cancelled { output_may_exist });
        }
        Err(BackupRunError::Failed(error)) => {
            let _ = messages.send(BackupWorkerMessage::Failed { mode, error });
        }
    }
}

#[cfg(any(not(feature = "non-elevated-tests"), test))]
fn direct_failure_is_cancelled(operation_failed: bool, cancel_requested: &AtomicBool) -> bool {
    operation_failed && cancel_requested.load(Ordering::SeqCst)
}

#[cfg(not(feature = "non-elevated-tests"))]
enum BackupRunError {
    Cancelled { output_may_exist: bool },
    Failed(String),
}

#[cfg(not(feature = "non-elevated-tests"))]
fn run_direct(
    authorized: AuthorizedDirectBackupIntent,
    _ghost: Option<Ghost>,
    cancel_requested: Arc<AtomicBool>,
    messages: &Sender<BackupWorkerMessage>,
) -> Result<(), String> {
    let intent = authorized.intent;
    let bound = rebind_authorized_backup_paths(&intent.config, authorized.authorization)?;
    let parent = bound
        .target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "backup destination has no parent".to_owned())?;
    let metadata = std::fs::symlink_metadata(parent)
        .map_err(|error| format!("inspect backup destination parent: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("backup destination parent is not a plain directory".to_owned());
    }
    let config = &intent.config;
    let handoff = config
        .handoff
        .as_ref()
        .ok_or_else(|| "authorized backup has no LRBK2 handoff".to_owned())?;
    use lr_core::backup_atomic_publish::ExistingPublishKind;
    use lr_core::backup_handoff::BackupOutputPolicy;
    let existing_kind = match handoff.output_policy {
        BackupOutputPolicy::Create => None,
        BackupOutputPolicy::Replace => Some(ExistingPublishKind::Replace),
        BackupOutputPolicy::Append => Some(ExistingPublishKind::Append),
    };
    let task_requests_append = matches!(
        &intent.task,
        DirectBackupTaskKind::Wim {
            append_if_destination_exists: true
        } | DirectBackupTaskKind::Esd {
            append_if_destination_exists: true
        }
    );
    if task_requests_append != (handoff.output_policy == BackupOutputPolicy::Append) {
        return Err("backup task intent does not match its authenticated output policy".to_owned());
    }
    let session_id = lr_core::handoff_auth::SessionId::parse(&handoff.session_id)
        .map_err(|error| format!("validate backup publication session: {error}"))?;
    let target_name = bound
        .target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "backup destination filename is not valid Unicode".to_owned())?
        .to_owned();
    let staged_extension = bound
        .target
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "backup destination omits its image extension".to_owned())?
        .to_ascii_lowercase();
    if let Some(recovery_session) =
        lr_core::backup_atomic_publish::SecurePublishSession::open_if_present(parent, &session_id)
            .map_err(|error| format!("inspect interrupted backup publication: {error}"))?
    {
        let outcome = match existing_kind {
            Some(kind) => lr_core::backup_atomic_publish::recover_existing(
                &recovery_session,
                kind,
                &target_name,
                &staged_extension,
            ),
            None => lr_core::backup_atomic_publish::recover_create(
                &recovery_session,
                &target_name,
                &staged_extension,
            ),
        }
        .map_err(|error| format!("recover interrupted backup publication: {error}"))?;
        if outcome == lr_core::backup_atomic_publish::RecoveryOutcome::Committed {
            if let Err(error) = recovery_session.remove_empty() {
                log::warn!(
                    "committed backup was recovered but its empty private session remains: {error}"
                );
            }
            return Ok(());
        }
        recovery_session
            .remove_empty()
            .map_err(|error| format!("remove rolled-back backup publication session: {error}"))?;
    }

    if existing_kind.is_none() && bound.target.exists() {
        return Err("create-only destination appeared before capture".to_owned());
    }

    let mut publish_session =
        lr_core::backup_atomic_publish::SecurePublishSession::create(parent, &session_id)
            .map_err(|error| format!("create secure backup publication session: {error}"))?;
    let staged = publish_session
        .path()
        .join(format!("staged.{staged_extension}"));
    let (existing_preparation, append_base_catalog) = match existing_kind {
        Some(kind) => {
            let prepared = (|| {
                let mut preparation = publish_session
                    .prepare_existing_copy(&target_name, &staged_extension)
                    .map_err(|error| format!("prepare locked existing backup copy: {error}"))?;
                let expected = handoff.base_file.as_ref().ok_or_else(|| {
                    "existing backup authorization omits its base identity".to_owned()
                })?;
                let observed = preparation.old_expectation();
                if observed.length != expected.length_bytes || observed.sha256 != expected.sha256 {
                    return Err(
                        "locked existing backup does not match its authenticated base identity"
                            .to_owned(),
                    );
                }
                let base_catalog = match kind {
                    ExistingPublishKind::Append => Some(
                        publish_session
                            .inspect_prepared_staged(&mut preparation, |path| {
                                Dism::read_verified_backup_catalog(path)
                            })
                            .map_err(|error| {
                                format!("inspect locked append base catalog: {error}")
                            })?,
                    ),
                    ExistingPublishKind::Replace => {
                        publish_session
                            .discard_copied_staged_for_replace(&mut preparation)
                            .map_err(|error| {
                                format!("reset copied staging for replacement: {error}")
                            })?;
                        None
                    }
                };
                Ok((preparation, base_catalog))
            })();
            match prepared {
                Ok((preparation, base_catalog)) => (Some(preparation), base_catalog),
                Err(error) => {
                    let cleanup = lr_core::backup_atomic_publish::recover_existing(
                        &publish_session,
                        kind,
                        &target_name,
                        &staged_extension,
                    )
                    .and_then(|_| publish_session.remove_empty());
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(format!(
                            "{error}; exact existing-session rollback also failed: {cleanup_error:#}"
                        )),
                    };
                }
            }
        }
        None => (None, None),
    };
    let (dism_tx, dism_rx) = mpsc::channel::<DismProgress>();
    let relay_messages = messages.clone();
    let relay_cancel = Arc::clone(&cancel_requested);
    let relay = std::thread::spawn(move || {
        let mut cancellation_reported = false;
        while let Ok(progress) = dism_rx.recv() {
            if relay_cancel.load(Ordering::SeqCst) && !cancellation_reported {
                cancellation_reported = true;
                let _ = relay_messages.send(BackupWorkerMessage::CancellationRequested {
                    operation_may_still_be_running: true,
                });
            }
            let _ = relay_messages.send(BackupWorkerMessage::Progress {
                percentage: progress.percentage,
                status: progress.status,
            });
        }
    });

    let staged_text = staged.to_string_lossy();
    let result = match &intent.task {
        DirectBackupTaskKind::Wim {
            append_if_destination_exists: _,
        } => {
            let dism = Dism::new();
            dism.capture_image_staged(
                &staged_text,
                &bound.source_root,
                &config.name,
                &config.description,
                false,
                Some(dism_tx.clone()),
            )
        }
        DirectBackupTaskKind::Esd {
            append_if_destination_exists: _,
        } => {
            let dism = Dism::new();
            dism.capture_image_staged(
                &staged_text,
                &bound.source_root,
                &config.name,
                &config.description,
                true,
                Some(dism_tx.clone()),
            )
        }
        DirectBackupTaskKind::Swm { .. } => Err(anyhow::anyhow!(
            "transactional stable-volume SWM backup is not enabled"
        )),
        DirectBackupTaskKind::Ghost => Err(anyhow::anyhow!(
            "transactional stable-volume Ghost backup is not enabled"
        )),
    };

    drop(dism_tx);
    let _ = relay.join();
    let prepared = (|| -> Result<lr_core::backup_atomic_publish::FileExpectation, String> {
        result.map_err(|error| error.to_string())?;
        let completed_catalog = Dism::read_verified_backup_catalog(&staged)
            .map_err(|error| format!("verify completed backup staging: {error}"))?;
        match append_base_catalog.as_ref() {
            Some(base) => lr_core::backup_image_catalog::verify_append_catalog(
                base,
                &completed_catalog,
                &config.name,
                &config.description,
            ),
            None => lr_core::backup_image_catalog::verify_fresh_catalog(
                &completed_catalog,
                &config.name,
                &config.description,
            ),
        }
        .map_err(|error| format!("verify completed backup catalog semantics: {error}"))?;
        lr_core::scoped_temp_file::restrict_to_system_and_administrators(&staged)
            .map_err(|error| format!("seal completed backup staging custody: {error}"))?;
        let completed = publish_session
            .inspect_staged_file(&staged_extension)
            .map_err(|error| format!("lock and hash completed backup staging: {error}"))?;
        Ok(completed)
    })();
    let completed = match prepared {
        Ok(completed) => completed,
        Err(error) => {
            drop(existing_preparation);
            let cleanup = match existing_kind {
                Some(kind) => lr_core::backup_atomic_publish::recover_existing(
                    &publish_session,
                    kind,
                    &target_name,
                    &staged_extension,
                ),
                None => lr_core::backup_atomic_publish::recover_create(
                    &publish_session,
                    &target_name,
                    &staged_extension,
                ),
            }
            .and_then(|_| publish_session.remove_empty());
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; exact unprepared-session rollback also failed: {cleanup_error:#}"
                )),
            };
        }
    };
    match (existing_kind, existing_preparation) {
        (Some(kind), Some(preparation)) => lr_core::backup_atomic_publish::publish_existing(
            &mut publish_session,
            kind,
            preparation,
            completed,
        ),
        (None, None) => lr_core::backup_atomic_publish::publish_create(
            &mut publish_session,
            &target_name,
            &staged_extension,
            completed,
        ),
        _ => Err(anyhow::anyhow!(
            "backup publication preparation does not match its authenticated policy"
        )),
    }
    .map_err(|error| format!("publish completed backup with durable CAS: {error}"))?;
    if let Err(error) = publish_session.remove_empty() {
        log::warn!("backup CAS committed but its empty private session remains: {error}");
    }
    Ok(())
}

#[cfg(not(feature = "non-elevated-tests"))]
struct BoundBackupPaths {
    source_root: String,
    destination_root: String,
    target: PathBuf,
}

#[cfg(not(feature = "non-elevated-tests"))]
fn rebind_authorized_backup_paths(
    config: &crate::core::install_config::BackupConfig,
    authorization: crate::core::native_backup_controller::DirectBackupStableAuthorization,
) -> Result<BoundBackupPaths, String> {
    let handoff = config
        .handoff
        .as_ref()
        .ok_or_else(|| "backup configuration has no LRBK2 authorization".to_owned())?;
    let disk_numbers = lr_core::windows_storage::physical_disk_numbers()
        .map_err(|error| format!("enumerate physical disks for backup rebind: {error}"))?;
    let mut candidates = Vec::with_capacity(disk_numbers.len());
    for disk_number in disk_numbers {
        let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)
            .map_err(|error| format!("snapshot physical disk {disk_number}: {error}"))?;
        candidates.push((disk_number, snapshot));
    }
    let (source_disk, destination_disk) =
        lr_core::backup_handoff::bind_unique_backup_volumes(handoff, &candidates)
            .map_err(|error| error.to_string())?;
    if source_disk != authorization.source.extent.disk_number
        || destination_disk != authorization.destination.extent.disk_number
        || handoff.source.partition_offset_bytes != authorization.source.extent.offset_bytes
        || handoff.destination.partition_offset_bytes
            != authorization.destination.extent.offset_bytes
        || handoff.source.partition_length_bytes != authorization.source.extent.extent_length_bytes
        || handoff.destination.partition_length_bytes
            != authorization.destination.extent.extent_length_bytes
    {
        return Err("backup authorization no longer matches rebound physical volumes".to_owned());
    }
    let source_root = lr_core::windows_storage::volume_guid_path_for_partition(
        source_disk,
        handoff.source.partition_offset_bytes,
    )
    .map_err(|error| format!("resolve backup source volume GUID: {error}"))?;
    let destination_root = lr_core::windows_storage::volume_guid_path_for_partition(
        destination_disk,
        handoff.destination.partition_offset_bytes,
    )
    .map_err(|error| format!("resolve backup destination volume GUID: {error}"))?;
    for (role, root) in [("source", &source_root), ("destination", &destination_root)] {
        let status = strict_bitlocker_status(root)?;
        if !matches!(
            status,
            crate::core::bitlocker::VolumeStatus::NotEncrypted
                | crate::core::bitlocker::VolumeStatus::EncryptedUnlocked
        ) {
            return Err(format!(
                "backup {role} BitLocker state changed inside worker: {status:?}"
            ));
        }
    }
    let target = PathBuf::from(&destination_root).join(&handoff.destination_relative_path);
    Ok(BoundBackupPaths {
        source_root,
        destination_root,
        target,
    })
}

#[cfg(any(not(feature = "non-elevated-tests"), test))]
fn verify_direct_output_with(
    path: &Path,
    task: &DirectBackupTaskKind,
    metadata_reader: &impl DirectOutputMetadataReader,
) -> Result<(), String> {
    let role = if matches!(task, DirectBackupTaskKind::Swm { .. }) {
        crate::tr!("SWM 首卷")
    } else {
        crate::tr!("备份输出文件")
    };
    let metadata = metadata_reader.read_metadata(path).map_err(|error| {
        crate::tr!(
            "备份输出复验失败：无法读取 {} 的文件元数据：{}",
            role,
            error
        )
    })?;
    if !metadata.is_regular_file {
        return Err(crate::tr!("备份输出复验失败：{} 不是普通文件", role));
    }
    if metadata.length == 0 {
        return Err(crate::tr!("备份输出复验失败：{} 为空文件", role));
    }
    Ok(())
}

#[cfg(not(feature = "non-elevated-tests"))]
fn run_pe_handoff(
    authorized: AuthorizedPeBackupIntent,
    cancel_requested: &AtomicBool,
    messages: &Sender<BackupWorkerMessage>,
) -> Result<(), BackupRunError> {
    let intent = authorized.intent;
    send_progress(messages, 10, &crate::tr!("正在验证 PE 环境"));
    let pe_snapshot = require_verified_cached_pe(&intent.pe).map_err(BackupRunError::Failed)?;
    stop_before_next_stage(cancel_requested)?;

    let bound = rebind_authorized_backup_paths(&intent.config, authorized.authorization)
        .map_err(BackupRunError::Failed)?;
    let source_status =
        strict_bitlocker_status(&bound.source_root).map_err(BackupRunError::Failed)?;
    let destination_status =
        strict_bitlocker_status(&bound.destination_root).map_err(BackupRunError::Failed)?;
    if source_status != crate::core::bitlocker::VolumeStatus::NotEncrypted
        || destination_status != crate::core::bitlocker::VolumeStatus::NotEncrypted
    {
        return Err(BackupRunError::Failed(
            "ViaPE backup volumes must remain unencrypted immediately before handoff".to_owned(),
        ));
    }
    let destination_letter =
        lr_core::windows_storage::path_drive_letter(Path::new(&intent.config.save_path))
            .ok_or_else(|| {
                BackupRunError::Failed("backup destination has no local drive".to_owned())
            })?;
    let current_destination = lr_core::windows_storage::stable_volume_identity(destination_letter)
        .map_err(|error| BackupRunError::Failed(format!("recheck backup data volume: {error}")))?;
    if !lr_core::windows_storage::same_stable_volume_identity(
        authorized.authorization.destination,
        current_destination,
    ) {
        return Err(BackupRunError::Failed(
            "backup destination drive letter changed before handoff".to_owned(),
        ));
    }
    let source_letter =
        lr_core::windows_storage::path_drive_letter(Path::new(&intent.config.source_partition))
            .ok_or_else(|| BackupRunError::Failed("backup source has no local drive".to_owned()))?;
    let current_source =
        lr_core::windows_storage::stable_volume_identity(source_letter).map_err(|error| {
            BackupRunError::Failed(format!("recheck backup source volume: {error}"))
        })?;
    if !lr_core::windows_storage::same_stable_volume_identity(
        authorized.authorization.source,
        current_source,
    ) {
        return Err(BackupRunError::Failed(
            "backup source drive letter changed before authenticated handoff".to_owned(),
        ));
    }
    let data_partition = format!("{}:", destination_letter);
    send_progress(messages, 30, &crate::tr!("正在暂存备份配置"));
    let handoff_auth_key = lr_core::handoff_auth::SessionAuthKey::generate().map_err(|error| {
        BackupRunError::Failed(format!("generate authenticated PE handoff key: {error}"))
    })?;
    let mut transaction = ConfigFileManager::write_backup_config_transactional(
        &intent.config.source_partition,
        &data_partition,
        &intent.config,
        &handoff_auth_key,
    )
    .map_err(|error| BackupRunError::Failed(format!("备份配置写入失败: {error}")))?;

    if let Err(cancelled) = stop_before_next_stage(cancel_requested) {
        if let Err(error) = transaction.rollback() {
            log::error!("取消 PE 备份交接时回滚暂存配置失败: {error}");
        }
        return Err(cancelled);
    }
    send_progress(messages, 60, &crate::tr!("正在安装 PE 启动项"));
    let _ = messages.send(BackupWorkerMessage::PeCommitStarted);
    let session_id = &intent
        .config
        .handoff
        .as_ref()
        .ok_or_else(|| BackupRunError::Failed("missing LRBK2 session".to_owned()))?
        .session_id;
    let config_bytes = transaction.take_boot_config_bytes().map_err(|error| {
        BackupRunError::Failed(format!("take authenticated backup config: {error}"))
    })?;
    let manifest_bytes = transaction.take_boot_manifest_bytes().map_err(|error| {
        BackupRunError::Failed(format!("take authenticated backup manifest: {error}"))
    })?;
    let payload = crate::core::pe::HandoffBootPayload::new(
        handoff_auth_key,
        lr_core::handoff_auth::HandoffPurpose::Backup,
        session_id,
        config_bytes,
        manifest_bytes,
        None,
        None,
    )
    .map_err(|error| {
        BackupRunError::Failed(format!("build authenticated backup boot payload: {error}"))
    })?;
    if let Err(error) = crate::core::pe::PeManager::new()
        .boot_to_pe_for_backup(
            &pe_snapshot.path.to_string_lossy(),
            &intent.pe.display_name,
            payload,
        )
        .and_then(|transaction| transaction.commit())
    {
        let rollback = transaction.rollback();
        let detail = match rollback {
            Ok(()) => format!("PE 启动项安装失败，备份配置已回滚: {error}"),
            Err(rollback_error) => {
                format!("PE 启动项安装失败: {error}; 备份配置回滚也失败: {rollback_error}")
            }
        };
        return Err(BackupRunError::Failed(detail));
    }
    send_progress(messages, 100, &crate::tr!("PE 备份准备完成"));
    Ok(())
}

#[cfg(not(feature = "non-elevated-tests"))]
fn require_verified_cached_pe(
    pe: &crate::download::config::OnlinePE,
) -> Result<crate::core::pe::LocalPeSnapshot, String> {
    use lr_core::cached_artifact::CachedArtifactStatus;

    match crate::core::pe::PeManager::check_cached_pe(
        &pe.filename,
        pe.sha256.as_deref(),
        pe.md5.as_deref(),
    ) {
        Ok(CachedArtifactStatus::Ready { path, .. }) => {
            crate::core::pe::snapshot_local_pe(&path, &pe.filename)
                .map_err(|error| format!("snapshot local PE payload: {error}"))
        }
        Ok(CachedArtifactStatus::Missing) => Err(format!("PE 文件不存在: {}", pe.filename)),
        Err(error) => Err(format!("PE 文件不可用: {error}")),
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn stop_before_next_stage(cancel_requested: &AtomicBool) -> Result<(), BackupRunError> {
    if cancel_requested.load(Ordering::SeqCst) {
        Err(BackupRunError::Cancelled {
            output_may_exist: false,
        })
    } else {
        Ok(())
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn send_progress(messages: &Sender<BackupWorkerMessage>, percentage: u8, status: &str) {
    let _ = messages.send(BackupWorkerMessage::Progress {
        percentage,
        status: status.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::install_config::BackupConfig;
    use crate::core::native_backup_controller::{
        plan_backup_launch, BackupLaunchIntent, DirectBackupTaskKind,
    };

    fn config(format: u8) -> BackupConfig {
        let extension = match format {
            1 => "esd",
            2 => "swm",
            3 => "gho",
            _ => "wim",
        };
        BackupConfig {
            save_path: format!("D:\\backup.{extension}"),
            name: "System Backup".to_owned(),
            description: "LetRecovery backup".to_owned(),
            source_partition: "C:".to_owned(),
            incremental: true,
            format,
            swm_split_size: 4096,
            wim_engine: 1,
            handoff: None,
        }
    }

    struct FakeMetadataReader(Result<DirectOutputMetadata, std::io::ErrorKind>);

    impl DirectOutputMetadataReader for FakeMetadataReader {
        fn read_metadata(&self, _path: &Path) -> std::io::Result<DirectOutputMetadata> {
            self.0.map_err(std::io::Error::from)
        }
    }

    #[test]
    fn worker_message_keeps_terminal_state_explicit() {
        assert_ne!(
            BackupWorkerMessage::Completed {
                mode: BackupLaunchMode::Direct
            },
            BackupWorkerMessage::Cancelled {
                output_may_exist: true
            }
        );
    }

    #[test]
    fn a_direct_backend_error_after_user_cancellation_is_reported_as_cancelled() {
        let cancel = AtomicBool::new(false);
        assert!(!direct_failure_is_cancelled(true, &cancel));
        cancel.store(true, Ordering::SeqCst);
        assert!(direct_failure_is_cancelled(true, &cancel));
        assert!(!direct_failure_is_cancelled(false, &cancel));
    }

    #[test]
    fn direct_output_verification_accepts_a_nonempty_regular_file() {
        let reader = FakeMetadataReader(Ok(DirectOutputMetadata {
            is_regular_file: true,
            length: 4096,
        }));
        assert!(verify_direct_output_with(
            Path::new("D:\\backup.wim"),
            &DirectBackupTaskKind::Wim {
                append_if_destination_exists: false,
            },
            &reader,
        )
        .is_ok());
    }

    #[test]
    fn direct_output_verification_rejects_missing_non_regular_and_empty_outputs() {
        let missing = FakeMetadataReader(Err(std::io::ErrorKind::NotFound));
        assert!(verify_direct_output_with(
            Path::new("D:\\backup.gho"),
            &DirectBackupTaskKind::Ghost,
            &missing,
        )
        .unwrap_err()
        .contains("备份输出复验失败"));

        for metadata in [
            DirectOutputMetadata {
                is_regular_file: false,
                length: 4096,
            },
            DirectOutputMetadata {
                is_regular_file: true,
                length: 0,
            },
        ] {
            let reader = FakeMetadataReader(Ok(metadata));
            assert!(verify_direct_output_with(
                Path::new("D:\\backup.esd"),
                &DirectBackupTaskKind::Esd {
                    append_if_destination_exists: false,
                },
                &reader,
            )
            .is_err());
        }
    }

    #[test]
    fn swm_output_verification_reads_the_requested_first_volume() {
        struct RecordingReader(std::sync::Mutex<Option<std::path::PathBuf>>);
        impl DirectOutputMetadataReader for RecordingReader {
            fn read_metadata(&self, path: &Path) -> std::io::Result<DirectOutputMetadata> {
                *self.0.lock().unwrap() = Some(path.to_path_buf());
                Ok(DirectOutputMetadata {
                    is_regular_file: true,
                    length: 8192,
                })
            }
        }

        let reader = RecordingReader(std::sync::Mutex::new(None));
        verify_direct_output_with(
            Path::new("D:\\system.swm"),
            &DirectBackupTaskKind::Swm {
                split_size_mb: 4096,
            },
            &reader,
        )
        .unwrap();
        assert_eq!(
            reader.0.lock().unwrap().as_deref(),
            Some(Path::new("D:\\system.swm"))
        );
    }

    #[test]
    fn controller_preserves_all_direct_format_dispatches() {
        let expected = [
            DirectBackupTaskKind::Wim {
                append_if_destination_exists: true,
            },
            DirectBackupTaskKind::Esd {
                append_if_destination_exists: true,
            },
            DirectBackupTaskKind::Swm {
                split_size_mb: 4096,
            },
            DirectBackupTaskKind::Ghost,
        ];
        for (format, expected) in expected.into_iter().enumerate() {
            let plan = plan_backup_launch(&config(format as u8), false, false, None).unwrap();
            let BackupLaunchIntent::Direct(intent) = plan.intent else {
                panic!("expected a direct backup intent");
            };
            assert_eq!(intent.task, expected);
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_feature_fails_closed_before_starting_a_worker() {
        let intent = plan_backup_launch(&config(0), false, false, None)
            .unwrap()
            .intent;
        assert!(matches!(
            execute_backup(intent),
            Err(BackupExecutionError::DisabledInDevelopment)
        ));
    }
}
