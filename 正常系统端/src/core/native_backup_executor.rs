//! Worker boundary for backup launch intents emitted by the native UI.
//!
//! The native window owns presentation state; this module owns background execution and reports
//! typed messages.  It deliberately keeps reboot/shutdown outside this boundary.  A PE handoff
//! only verifies the selected cached artifact, installs its boot entry and writes the existing
//! backward-compatible backup configuration.

#[cfg(any(not(feature = "non-elevated-tests"), test))]
use std::path::Path;
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
    let (messages_tx, messages) = mpsc::channel();
    let cancel_requested = Arc::new(AtomicBool::new(false));
    let cancel_for_worker = Arc::clone(&cancel_requested);

    // Create Ghost before moving into the worker so the UI handle can share its real cancel flag.
    let ghost = matches!(
        intent,
        BackupLaunchIntent::Direct(DirectBackupIntent {
            task: DirectBackupTaskKind::Ghost,
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
    intent: BackupLaunchIntent,
    ghost: Option<Ghost>,
    cancel_requested: Arc<AtomicBool>,
    messages: Sender<BackupWorkerMessage>,
) {
    let mode = match &intent {
        BackupLaunchIntent::Direct(_) => BackupLaunchMode::Direct,
        BackupLaunchIntent::ViaPe(_) => BackupLaunchMode::ViaPe,
    };
    let _ = messages.send(BackupWorkerMessage::Started { mode });

    if cancel_requested.load(Ordering::SeqCst) {
        let _ = messages.send(BackupWorkerMessage::Cancelled {
            output_may_exist: false,
        });
        return;
    }

    let result: Result<(), BackupRunError> = match intent {
        BackupLaunchIntent::Direct(intent) => {
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
        BackupLaunchIntent::ViaPe(intent) => run_pe_handoff(intent, &cancel_requested, &messages),
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
    intent: DirectBackupIntent,
    ghost: Option<Ghost>,
    cancel_requested: Arc<AtomicBool>,
    messages: &Sender<BackupWorkerMessage>,
) -> Result<(), String> {
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

    let config = &intent.config;
    let result = match &intent.task {
        DirectBackupTaskKind::Wim {
            append_if_destination_exists,
        } => {
            let dism = Dism::new();
            if *append_if_destination_exists && Path::new(&config.save_path).exists() {
                dism.append_image(
                    &config.save_path,
                    &intent.capture_directory,
                    &config.name,
                    &config.description,
                    Some(dism_tx.clone()),
                )
            } else {
                dism.capture_image(
                    &config.save_path,
                    &intent.capture_directory,
                    &config.name,
                    &config.description,
                    Some(dism_tx.clone()),
                )
            }
        }
        DirectBackupTaskKind::Esd {
            append_if_destination_exists,
        } => {
            let dism = Dism::new();
            if *append_if_destination_exists && Path::new(&config.save_path).exists() {
                dism.append_image_esd(
                    &config.save_path,
                    &intent.capture_directory,
                    &config.name,
                    &config.description,
                    Some(dism_tx.clone()),
                )
            } else {
                dism.capture_image_esd(
                    &config.save_path,
                    &intent.capture_directory,
                    &config.name,
                    &config.description,
                    Some(dism_tx.clone()),
                )
            }
        }
        DirectBackupTaskKind::Swm { split_size_mb } => Dism::new().capture_image_swm(
            &config.save_path,
            &intent.capture_directory,
            &config.name,
            &config.description,
            *split_size_mb,
            Some(dism_tx.clone()),
        ),
        DirectBackupTaskKind::Ghost => ghost
            .ok_or_else(|| "Ghost 备份执行器未初始化".to_owned())?
            .create_image_from_letter(
                &config.source_partition,
                &config.save_path,
                Some(dism_tx.clone()),
            ),
    };

    drop(dism_tx);
    let _ = relay.join();
    result.map_err(|error| error.to_string())?;
    verify_direct_output_with(
        Path::new(&config.save_path),
        &intent.task,
        &SystemDirectOutputMetadataReader,
    )
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
    intent: PeBackupPreparationIntent,
    cancel_requested: &AtomicBool,
    messages: &Sender<BackupWorkerMessage>,
) -> Result<(), BackupRunError> {
    send_progress(messages, 10, &crate::tr!("正在验证 PE 环境"));
    let pe_path = require_verified_cached_pe(&intent.pe).map_err(BackupRunError::Failed)?;
    stop_before_next_stage(cancel_requested)?;

    let data_partition = find_backup_data_partition(&intent.config.source_partition)
        .map_err(BackupRunError::Failed)?;
    send_progress(messages, 30, &crate::tr!("正在暂存备份配置"));
    let transaction = ConfigFileManager::write_backup_config_transactional(
        &intent.config.source_partition,
        &data_partition,
        &intent.config,
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
    if let Err(error) = crate::core::pe::PeManager::new()
        .boot_to_pe(&pe_path.to_string_lossy(), &intent.pe.display_name)
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
) -> Result<std::path::PathBuf, String> {
    use lr_core::cached_artifact::CachedArtifactStatus;

    match crate::core::pe::PeManager::check_cached_pe(
        &pe.filename,
        pe.sha256.as_deref(),
        pe.md5.as_deref(),
    ) {
        Ok(CachedArtifactStatus::Ready { path, .. }) => Ok(path),
        Ok(CachedArtifactStatus::Missing) => Err(format!("PE 文件不存在: {}", pe.filename)),
        Err(error) => Err(format!("PE 文件安全校验失败: {error}")),
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

#[cfg(not(feature = "non-elevated-tests"))]
fn find_backup_data_partition(exclude_partition: &str) -> Result<String, String> {
    use crate::core::disk::DiskManager;

    let excluded = exclude_partition
        .chars()
        .next()
        .unwrap_or('C')
        .to_ascii_uppercase();
    for letter in 'A'..='Z' {
        if letter == excluded || letter == 'X' {
            continue;
        }
        if !Path::new(&format!("{letter}:\\")).exists()
            || DiskManager::is_cdrom(letter)
            || !DiskManager::is_fixed_drive(letter)
        {
            continue;
        }
        if DiskManager::get_free_space_bytes(&format!("{letter}:"))
            .is_some_and(|free| free >= 100 * 1024 * 1024)
        {
            return Ok(format!("{letter}:"));
        }
    }
    Err("没有找到可安全保存 PE 备份配置的数据分区".to_owned())
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
