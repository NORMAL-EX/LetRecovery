//! Native online-download worker boundary.
//!
//! The Win32 window supplies a validated [`DownloadPlan`] and consumes typed
//! messages.  This module is the only native-UI layer that starts aria2 or
//! verifies the resulting file.  A requested post-download action is returned
//! to the UI for explicit confirmation; it is never executed by the worker.

use std::fmt;
#[cfg(not(feature = "non-elevated-tests"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(not(feature = "non-elevated-tests"))]
use std::time::Duration;

#[cfg(any(test, not(feature = "non-elevated-tests")))]
use lr_core::download_integrity::IntegrityRequirement;
use lr_core::download_integrity::{
    validate_download_filename, validate_download_url, HashAlgorithm,
};
#[cfg(not(feature = "non-elevated-tests"))]
use lr_core::download_integrity::{verify_file, HashVerification};

use crate::core::native_download_controller::{DownloadCompletion, DownloadPlan};
#[cfg(not(feature = "non-elevated-tests"))]
use crate::download::aria2::{Aria2Manager, DownloadStatus};

#[cfg(not(feature = "non-elevated-tests"))]
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(300);
#[cfg(not(feature = "non-elevated-tests"))]
const MAX_STATUS_ERRORS: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadWorkerCommand {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntegrityOutcome {
    NotProvided,
    Passed(HashAlgorithm),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DownloadWorkerMessage {
    Starting,
    Progress {
        completed_bytes: u64,
        total_bytes: u64,
        bytes_per_second: u64,
        percentage: f64,
        paused: bool,
    },
    Verifying {
        algorithm: HashAlgorithm,
    },
    /// The UI may display `follow_up` as a confirmation action. It must not be
    /// performed merely because this message was received.
    Completed {
        path: PathBuf,
        integrity: IntegrityOutcome,
        follow_up: DownloadCompletion,
    },
    Cancelled,
    Failed(DownloadWorkerError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadFailureStage {
    Validation,
    Initialization,
    Transfer,
    Integrity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadWorkerError {
    pub stage: DownloadFailureStage,
    pub message: String,
}

impl DownloadWorkerError {
    #[cfg(not(feature = "non-elevated-tests"))]
    fn new(stage: DownloadFailureStage, error: impl fmt::Display) -> Self {
        Self {
            stage,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for DownloadWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for DownloadWorkerError {}

#[derive(Debug)]
pub struct DownloadWorker {
    commands: Sender<DownloadWorkerCommand>,
    messages: Receiver<DownloadWorkerMessage>,
}

impl DownloadWorker {
    pub fn send(
        &self,
        command: DownloadWorkerCommand,
    ) -> Result<(), mpsc::SendError<DownloadWorkerCommand>> {
        self.commands.send(command)
    }

    pub fn try_recv(&self) -> Result<DownloadWorkerMessage, mpsc::TryRecvError> {
        self.messages.try_recv()
    }

    pub fn receiver(&self) -> &Receiver<DownloadWorkerMessage> {
        &self.messages
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadStartError {
    DevelopmentBuildDisabled,
    InvalidPlan(String),
    WorkerSpawn(String),
}

impl fmt::Display for DownloadStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DevelopmentBuildDisabled => {
                formatter.write_str("downloads are disabled in non-elevated development builds")
            }
            Self::InvalidPlan(message) => write!(formatter, "invalid download plan: {message}"),
            Self::WorkerSpawn(message) => {
                write!(formatter, "could not start download worker: {message}")
            }
        }
    }
}

impl std::error::Error for DownloadStartError {}

pub struct NativeDownloadExecutor;

impl NativeDownloadExecutor {
    /// Starts a worker only after the plan has been revalidated. In
    /// `non-elevated-tests` this returns before creating a directory, file,
    /// runtime, thread, process, or network connection.
    pub fn start(plan: DownloadPlan) -> Result<DownloadWorker, DownloadStartError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = plan;
            Err(DownloadStartError::DevelopmentBuildDisabled)
        }

        #[cfg(not(feature = "non-elevated-tests"))]
        {
            validate_plan(&plan).map_err(DownloadStartError::InvalidPlan)?;
            let (command_tx, command_rx) = mpsc::channel();
            let (message_tx, message_rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("native-download".into())
                .spawn(move || run_worker(plan, command_rx, message_tx))
                .map_err(|error| DownloadStartError::WorkerSpawn(error.to_string()))?;
            Ok(DownloadWorker {
                commands: command_tx,
                messages: message_rx,
            })
        }
    }
}

fn validate_plan(plan: &DownloadPlan) -> Result<(), String> {
    validate_download_url(&plan.url, true).map_err(|error| error.to_string())?;
    validate_download_filename(&plan.filename).map_err(|error| error.to_string())?;
    if plan.save_directory.as_os_str().is_empty() {
        return Err("save directory is empty".into());
    }
    let destination = plan.save_directory.join(&plan.filename);
    if destination.parent() != Some(plan.save_directory.as_path()) {
        return Err("destination escapes save directory".into());
    }
    match &plan.completion {
        DownloadCompletion::None => {}
        DownloadCompletion::OpenSystemImage(path) | DownloadCompletion::RunDownloadedFile(path)
            if path != &destination =>
        {
            return Err("completion path does not match downloaded file".into());
        }
        DownloadCompletion::OpenSystemImage(_) | DownloadCompletion::RunDownloadedFile(_) => {}
    }
    Ok(())
}

#[cfg(not(feature = "non-elevated-tests"))]
fn run_worker(
    plan: DownloadPlan,
    commands: Receiver<DownloadWorkerCommand>,
    messages: Sender<DownloadWorkerMessage>,
) {
    if let Err(error) = std::fs::create_dir_all(&plan.save_directory) {
        send_failure(&messages, DownloadFailureStage::Initialization, error);
        return;
    }
    if messages.send(DownloadWorkerMessage::Starting).is_err() {
        return;
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            send_failure(&messages, DownloadFailureStage::Initialization, error);
            return;
        }
    };
    runtime.block_on(run_download(plan, commands, messages));
}

#[cfg(not(feature = "non-elevated-tests"))]
async fn run_download(
    plan: DownloadPlan,
    commands: Receiver<DownloadWorkerCommand>,
    messages: Sender<DownloadWorkerMessage>,
) {
    let mut aria2 = match Aria2Manager::start_with_download_threads(plan.download_threads).await {
        Ok(manager) => manager,
        Err(error) => {
            send_failure(&messages, DownloadFailureStage::Initialization, error);
            return;
        }
    };
    let save_directory = plan.save_directory.to_string_lossy();
    let gid = match aria2
        .add_download(&plan.url, &save_directory, Some(&plan.filename))
        .await
    {
        Ok(gid) => gid,
        Err(error) => {
            send_failure(&messages, DownloadFailureStage::Transfer, error);
            let _ = aria2.shutdown().await;
            return;
        }
    };

    let mut status_errors = 0;
    loop {
        while let Ok(command) = commands.try_recv() {
            let result = match command {
                DownloadWorkerCommand::Pause => aria2.pause(&gid).await,
                DownloadWorkerCommand::Resume => aria2.resume(&gid).await,
                DownloadWorkerCommand::Cancel => {
                    let _ = aria2.cancel(&gid).await;
                    let _ = aria2.shutdown().await;
                    let _ = messages.send(DownloadWorkerMessage::Cancelled);
                    return;
                }
            };
            if let Err(error) = result {
                send_failure(&messages, DownloadFailureStage::Transfer, error);
                let _ = aria2.shutdown().await;
                return;
            }
        }

        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
        match aria2.get_status(&gid).await {
            Ok(progress) => {
                status_errors = 0;
                let paused = progress.status == DownloadStatus::Paused;
                let completed = progress.status == DownloadStatus::Complete;
                if let DownloadStatus::Error(message) = &progress.status {
                    send_failure(&messages, DownloadFailureStage::Transfer, message);
                    break;
                }
                if messages
                    .send(DownloadWorkerMessage::Progress {
                        completed_bytes: progress.completed_length,
                        total_bytes: progress.total_length,
                        bytes_per_second: progress.download_speed,
                        percentage: progress.percentage,
                        paused,
                    })
                    .is_err()
                {
                    break;
                }
                if completed {
                    let _ = aria2.shutdown().await;
                    finish_download(&plan, &messages);
                    return;
                }
            }
            Err(error) => {
                status_errors += 1;
                if status_errors >= MAX_STATUS_ERRORS {
                    send_failure(&messages, DownloadFailureStage::Transfer, error);
                    break;
                }
            }
        }
    }
    let _ = aria2.shutdown().await;
}

#[cfg(not(feature = "non-elevated-tests"))]
fn finish_download(plan: &DownloadPlan, messages: &Sender<DownloadWorkerMessage>) {
    let path = plan.save_directory.join(&plan.filename);
    let integrity = match &plan.integrity {
        IntegrityRequirement::NotProvided => IntegrityOutcome::NotProvided,
        IntegrityRequirement::Required(expected) => {
            let algorithm = expected.algorithm();
            if messages
                .send(DownloadWorkerMessage::Verifying { algorithm })
                .is_err()
            {
                return;
            }
            match verify_file(&path, expected) {
                Ok(HashVerification::Passed { .. }) => IntegrityOutcome::Passed(algorithm),
                Ok(HashVerification::Mismatch {
                    expected, actual, ..
                }) => {
                    remove_rejected_file(&path);
                    send_failure(
                        messages,
                        DownloadFailureStage::Integrity,
                        format!("{algorithm:?} mismatch: expected {expected}, actual {actual}"),
                    );
                    return;
                }
                Err(error) => {
                    remove_rejected_file(&path);
                    send_failure(messages, DownloadFailureStage::Integrity, error);
                    return;
                }
            }
        }
    };
    let _ = messages.send(DownloadWorkerMessage::Completed {
        path,
        integrity,
        follow_up: plan.completion.clone(),
    });
}

#[cfg(not(feature = "non-elevated-tests"))]
fn remove_rejected_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!(
            "[INTEGRITY] could not remove rejected download {}: {}",
            path.display(),
            error
        ),
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn send_failure(
    messages: &Sender<DownloadWorkerMessage>,
    stage: DownloadFailureStage,
    error: impl fmt::Display,
) {
    let _ = messages.send(DownloadWorkerMessage::Failed(DownloadWorkerError::new(
        stage, error,
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> DownloadPlan {
        DownloadPlan {
            url: "https://example.com/windows.iso".into(),
            save_directory: PathBuf::from(r"D:\Downloads"),
            filename: "windows.iso".into(),
            integrity: IntegrityRequirement::NotProvided,
            completion: DownloadCompletion::None,
            download_threads: 16,
        }
    }

    #[test]
    fn rejects_completion_path_different_from_download() {
        let mut plan = plan();
        plan.completion = DownloadCompletion::OpenSystemImage(PathBuf::from(r"D:\other.iso"));
        assert!(validate_plan(&plan).is_err());
    }

    #[test]
    fn rejects_unsafe_filename_at_executor_boundary() {
        let mut plan = plan();
        plan.filename = r"..\windows.iso".into();
        assert!(validate_plan(&plan).is_err());
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_rejects_before_starting_worker() {
        assert_eq!(
            NativeDownloadExecutor::start(plan()).unwrap_err(),
            DownloadStartError::DevelopmentBuildDisabled
        );
    }
}
