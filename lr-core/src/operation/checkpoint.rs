use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scoped_temp_file::ScopedTempFile;

use super::{run_with_retry, OperationError, RetryPolicy, RetrySafety, ThreadSleeper};

pub const CHECKPOINT_SCHEMA_VERSION: u32 = 1;
const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024;
const MAX_STEPS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Install,
    Backup,
    Expand,
    Repair,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Running,
    Interrupted,
    Failed,
    Cancelled,
    Succeeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Interrupted,
    Failed,
    Skipped,
    Succeeded,
}

impl StepStatus {
    fn is_complete(self) -> bool {
        matches!(self, Self::Skipped | Self::Succeeded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDefinition {
    pub id: String,
    pub name: String,
    pub idempotent: bool,
}

impl StepDefinition {
    pub fn new(id: impl Into<String>, name: impl Into<String>, idempotent: bool) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            idempotent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCheckpoint {
    pub id: String,
    pub name: String,
    pub idempotent: bool,
    pub status: StepStatus,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<OperationError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TargetFingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_guid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_offset_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationCheckpoint {
    pub schema_version: u32,
    pub operation_id: String,
    pub kind: OperationKind,
    pub status: OperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<OperationError>,
    pub revision: u64,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetFingerprint>,
    pub steps: Vec<StepCheckpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepTransition {
    Applied,
    AlreadyComplete,
    AlreadyRunning,
}

impl OperationCheckpoint {
    pub fn new(
        operation_id: impl Into<String>,
        kind: OperationKind,
        definitions: impl IntoIterator<Item = StepDefinition>,
        now_unix_ms: u64,
    ) -> Result<Self, OperationError> {
        let checkpoint = Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            operation_id: operation_id.into(),
            kind,
            status: OperationStatus::Pending,
            current_step: None,
            last_error: None,
            revision: 0,
            created_unix_ms: now_unix_ms,
            updated_unix_ms: now_unix_ms,
            target: None,
            steps: definitions
                .into_iter()
                .map(|definition| StepCheckpoint {
                    id: definition.id,
                    name: definition.name,
                    idempotent: definition.idempotent,
                    status: StepStatus::Pending,
                    attempts: 0,
                    last_error: None,
                })
                .collect(),
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn with_target(mut self, target: TargetFingerprint) -> Result<Self, OperationError> {
        validate_target(&target)?;
        self.target = Some(target);
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), OperationError> {
        if self.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return Err(OperationError::validation(format!(
                "unsupported checkpoint schema version {}",
                self.schema_version
            )));
        }
        validate_identifier(&self.operation_id, "operation id")?;
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(OperationError::validation(format!(
                "checkpoint must contain between 1 and {MAX_STEPS} steps"
            )));
        }

        let mut ids = HashSet::with_capacity(self.steps.len());
        let mut running_steps = 0;
        for step in &self.steps {
            validate_identifier(&step.id, "step id")?;
            validate_display_name(&step.name, "step name")?;
            if !ids.insert(step.id.as_str()) {
                return Err(OperationError::validation(format!(
                    "duplicate checkpoint step id: {}",
                    step.id
                )));
            }
            if step.status == StepStatus::Running {
                running_steps += 1;
            }
        }
        if running_steps > 1 {
            return Err(OperationError::validation(
                "checkpoint cannot contain more than one running step",
            ));
        }
        if let Some(current) = &self.current_step {
            if !ids.contains(current.as_str()) {
                return Err(OperationError::validation(format!(
                    "current step is not declared: {current}"
                )));
            }
        }
        if self.status == OperationStatus::Succeeded
            && self.steps.iter().any(|step| !step.status.is_complete())
        {
            return Err(OperationError::validation(
                "a succeeded operation must have only completed or skipped steps",
            ));
        }
        if let Some(target) = &self.target {
            validate_target(target)?;
        }
        Ok(())
    }

    pub fn start(&mut self, now_unix_ms: u64) -> Result<(), OperationError> {
        match self.status {
            OperationStatus::Pending | OperationStatus::Interrupted | OperationStatus::Failed => {
                self.status = OperationStatus::Running;
                self.last_error = None;
                self.touch(now_unix_ms);
                Ok(())
            }
            OperationStatus::Running => Ok(()),
            OperationStatus::Cancelled | OperationStatus::Succeeded => Err(OperationError::state(
                "a completed or cancelled operation cannot be started",
            )),
        }
    }

    pub fn begin_step(
        &mut self,
        step_id: &str,
        now_unix_ms: u64,
    ) -> Result<StepTransition, OperationError> {
        let index = self.step_index(step_id)?;
        if self.steps[index].status.is_complete() {
            return Ok(StepTransition::AlreadyComplete);
        }
        if self.steps[index].status == StepStatus::Running {
            return Ok(StepTransition::AlreadyRunning);
        }
        if self.steps[..index]
            .iter()
            .any(|step| !step.status.is_complete())
        {
            return Err(OperationError::state(format!(
                "cannot start step {step_id} before previous steps complete"
            )));
        }

        let is_retry = matches!(
            self.steps[index].status,
            StepStatus::Failed | StepStatus::Interrupted
        );
        if is_retry && !self.steps[index].idempotent {
            return Err(OperationError::state(format!(
                "step {step_id} is non-idempotent and requires manual restart confirmation"
            )));
        }

        self.start(now_unix_ms)?;
        if self
            .steps
            .iter()
            .enumerate()
            .any(|(other, step)| other != index && step.status == StepStatus::Running)
        {
            return Err(OperationError::state(
                "another checkpoint step is already running",
            ));
        }

        let step = &mut self.steps[index];
        step.status = StepStatus::Running;
        step.attempts = step.attempts.saturating_add(1);
        step.last_error = None;
        self.current_step = Some(step_id.to_string());
        self.touch(now_unix_ms);
        Ok(StepTransition::Applied)
    }

    pub fn complete_step(
        &mut self,
        step_id: &str,
        now_unix_ms: u64,
    ) -> Result<StepTransition, OperationError> {
        let index = self.step_index(step_id)?;
        if self.steps[index].status.is_complete() {
            return Ok(StepTransition::AlreadyComplete);
        }
        if self.steps[index].status != StepStatus::Running {
            return Err(OperationError::state(format!(
                "step {step_id} is not running"
            )));
        }
        self.steps[index].status = StepStatus::Succeeded;
        self.steps[index].last_error = None;
        self.current_step = None;
        self.touch(now_unix_ms);
        Ok(StepTransition::Applied)
    }

    pub fn skip_step(&mut self, step_id: &str, now_unix_ms: u64) -> Result<(), OperationError> {
        let index = self.step_index(step_id)?;
        match self.steps[index].status {
            StepStatus::Pending => {
                self.steps[index].status = StepStatus::Skipped;
                self.steps[index].last_error = None;
                self.touch(now_unix_ms);
                Ok(())
            }
            StepStatus::Skipped => Ok(()),
            _ => Err(OperationError::state(format!(
                "step {step_id} can only be skipped while pending"
            ))),
        }
    }

    pub fn fail_step(
        &mut self,
        step_id: &str,
        error: OperationError,
        now_unix_ms: u64,
    ) -> Result<(), OperationError> {
        let index = self.step_index(step_id)?;
        if self.steps[index].status != StepStatus::Running {
            return Err(OperationError::state(format!(
                "step {step_id} is not running"
            )));
        }
        self.steps[index].status = StepStatus::Failed;
        self.steps[index].last_error = Some(error);
        self.status = OperationStatus::Failed;
        self.current_step = Some(step_id.to_string());
        self.last_error = self.steps[index].last_error.clone();
        self.touch(now_unix_ms);
        Ok(())
    }

    pub fn mark_failed(&mut self, error: OperationError, now_unix_ms: u64) {
        self.status = OperationStatus::Failed;
        self.last_error = Some(error);
        self.touch(now_unix_ms);
    }

    pub fn mark_interrupted(&mut self, now_unix_ms: u64) {
        for step in &mut self.steps {
            if step.status == StepStatus::Running {
                step.status = StepStatus::Interrupted;
            }
        }
        if matches!(
            self.status,
            OperationStatus::Pending | OperationStatus::Running
        ) {
            self.status = OperationStatus::Interrupted;
            self.touch(now_unix_ms);
        }
    }

    pub fn mark_cancelled(&mut self, now_unix_ms: u64) -> Result<(), OperationError> {
        if self.status == OperationStatus::Succeeded {
            return Err(OperationError::state(
                "a succeeded operation cannot be cancelled",
            ));
        }
        self.status = OperationStatus::Cancelled;
        self.last_error = None;
        self.touch(now_unix_ms);
        Ok(())
    }

    pub fn mark_succeeded(&mut self, now_unix_ms: u64) -> Result<(), OperationError> {
        if self.steps.iter().any(|step| !step.status.is_complete()) {
            return Err(OperationError::state(
                "cannot complete an operation with unfinished steps",
            ));
        }
        self.status = OperationStatus::Succeeded;
        self.current_step = None;
        self.last_error = None;
        self.touch(now_unix_ms);
        Ok(())
    }

    pub fn step(&self, step_id: &str) -> Option<&StepCheckpoint> {
        self.steps.iter().find(|step| step.id == step_id)
    }

    fn step_index(&self, step_id: &str) -> Result<usize, OperationError> {
        self.steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| {
                OperationError::validation(format!("unknown checkpoint step: {step_id}"))
            })
    }

    fn touch(&mut self, now_unix_ms: u64) {
        self.updated_unix_ms = now_unix_ms.max(self.updated_unix_ms);
        self.revision = self.revision.saturating_add(1);
    }
}

#[derive(Debug, Clone)]
pub struct CheckpointStore {
    path: PathBuf,
}

impl CheckpointStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<OperationCheckpoint>, OperationError> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(OperationError::io("read checkpoint metadata", &error)),
        };
        if !metadata.is_file() {
            return Err(OperationError::validation(
                "checkpoint path is not a regular file",
            ));
        }
        if metadata.len() > MAX_CHECKPOINT_BYTES {
            return Err(OperationError::validation(format!(
                "checkpoint exceeds {MAX_CHECKPOINT_BYTES} bytes"
            )));
        }
        let bytes =
            fs::read(&self.path).map_err(|error| OperationError::io("read checkpoint", &error))?;
        let checkpoint: OperationCheckpoint = serde_json::from_slice(&bytes).map_err(|error| {
            OperationError::validation(format!("invalid checkpoint JSON: {error}"))
        })?;
        checkpoint.validate()?;
        Ok(Some(checkpoint))
    }

    pub fn save(&self, checkpoint: &OperationCheckpoint) -> Result<(), OperationError> {
        checkpoint.validate()?;
        let data = serde_json::to_vec_pretty(checkpoint).map_err(|error| {
            OperationError::new(
                super::OperationErrorKind::State,
                None::<String>,
                format!("serialize checkpoint: {error}"),
                false,
            )
        })?;
        write_atomic(&self.path, &data)
            .map_err(|error| OperationError::io("write checkpoint", &error))
    }

    pub fn remove(&self) -> Result<(), OperationError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(OperationError::io("remove checkpoint", &error)),
        }
    }
}

/// Transactional in-memory state paired with an atomic checkpoint store.
///
/// Every mutation is first applied to a clone and persisted. The live state is
/// replaced only after the write succeeds, so callers never observe a state
/// that was not durably recorded.
pub struct OperationJournal {
    store: CheckpointStore,
    checkpoint: OperationCheckpoint,
    write_retry: RetryPolicy,
}

impl OperationJournal {
    pub fn create(
        store: CheckpointStore,
        checkpoint: OperationCheckpoint,
    ) -> Result<Self, OperationError> {
        let journal = Self {
            store,
            checkpoint,
            write_retry: RetryPolicy::transient_io(),
        };
        journal.persist(&journal.checkpoint)?;
        Ok(journal)
    }

    pub fn open(store: CheckpointStore) -> Result<Option<Self>, OperationError> {
        let Some(checkpoint) = store.load()? else {
            return Ok(None);
        };
        Ok(Some(Self {
            store,
            checkpoint,
            write_retry: RetryPolicy::transient_io(),
        }))
    }

    pub fn checkpoint(&self) -> &OperationCheckpoint {
        &self.checkpoint
    }

    pub fn path(&self) -> &Path {
        self.store.path()
    }

    pub fn observe_step(
        &mut self,
        step_id: &str,
        now_unix_ms: u64,
    ) -> Result<StepTransition, OperationError> {
        self.update(|checkpoint| {
            if checkpoint.current_step.as_deref() == Some(step_id) {
                return Ok(StepTransition::AlreadyRunning);
            }
            if let Some(current) = checkpoint.current_step.clone() {
                if checkpoint
                    .step(&current)
                    .is_some_and(|step| step.status == StepStatus::Running)
                {
                    checkpoint.complete_step(&current, now_unix_ms)?;
                }
            }
            let index = checkpoint.step_index(step_id)?;
            let pending_ids: Vec<String> = checkpoint.steps[..index]
                .iter()
                .filter(|step| step.status == StepStatus::Pending)
                .map(|step| step.id.clone())
                .collect();
            for pending in pending_ids {
                checkpoint.skip_step(&pending, now_unix_ms)?;
            }
            checkpoint.begin_step(step_id, now_unix_ms)
        })
    }

    pub fn fail_current(
        &mut self,
        error: OperationError,
        now_unix_ms: u64,
    ) -> Result<(), OperationError> {
        self.update(|checkpoint| {
            if let Some(current) = checkpoint.current_step.clone() {
                checkpoint.fail_step(&current, error, now_unix_ms)
            } else {
                checkpoint.mark_failed(error, now_unix_ms);
                Ok(())
            }
        })
    }

    pub fn complete(&mut self, now_unix_ms: u64) -> Result<(), OperationError> {
        self.update(|checkpoint| {
            if let Some(current) = checkpoint.current_step.clone() {
                if checkpoint
                    .step(&current)
                    .is_some_and(|step| step.status == StepStatus::Running)
                {
                    checkpoint.complete_step(&current, now_unix_ms)?;
                }
            }
            let pending_ids: Vec<String> = checkpoint
                .steps
                .iter()
                .filter(|step| step.status == StepStatus::Pending)
                .map(|step| step.id.clone())
                .collect();
            for pending in pending_ids {
                checkpoint.skip_step(&pending, now_unix_ms)?;
            }
            checkpoint.mark_succeeded(now_unix_ms)
        })
    }

    pub fn mark_interrupted(&mut self, now_unix_ms: u64) -> Result<(), OperationError> {
        self.update(|checkpoint| {
            checkpoint.mark_interrupted(now_unix_ms);
            Ok(())
        })
    }

    pub fn remove(&self) -> Result<(), OperationError> {
        self.store.remove()
    }

    fn update<T>(
        &mut self,
        update: impl FnOnce(&mut OperationCheckpoint) -> Result<T, OperationError>,
    ) -> Result<T, OperationError> {
        let mut next = self.checkpoint.clone();
        let result = update(&mut next)?;
        self.persist(&next)?;
        self.checkpoint = next;
        Ok(result)
    }

    fn persist(&self, checkpoint: &OperationCheckpoint) -> Result<(), OperationError> {
        run_with_retry(
            &self.write_retry,
            RetrySafety::Idempotent,
            &ThreadSleeper,
            |_| self.store.save(checkpoint),
        )
    }
}

pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = ScopedTempFile::create_in(parent, "lr-atomic", "tmp", data)?;
    OpenOptions::new()
        .write(true)
        .open(temporary.path())?
        .sync_all()?;
    replace_file(temporary.path(), path)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let source = wide(source.as_os_str());
    let destination = wide(destination.as_os_str());
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| io::Error::other(format!("atomic replace failed: {error}")))
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn validate_identifier(value: &str, field: &str) -> Result<(), OperationError> {
    if value.is_empty() || value.len() > 128 {
        return Err(OperationError::validation(format!(
            "{field} must contain 1 to 128 characters"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(OperationError::validation(format!(
            "{field} contains unsupported characters"
        )));
    }
    Ok(())
}

fn validate_display_name(value: &str, field: &str) -> Result<(), OperationError> {
    if value.trim().is_empty() || value.len() > 256 || value.contains(['\r', '\n', '\0']) {
        return Err(OperationError::validation(format!(
            "{field} is empty, too long, or contains a control character"
        )));
    }
    Ok(())
}

fn validate_target(target: &TargetFingerprint) -> Result<(), OperationError> {
    for (field, value) in [
        ("disk serial", target.disk_serial.as_deref()),
        ("volume guid", target.volume_guid.as_deref()),
    ] {
        if let Some(value) = value {
            validate_display_name(value, field)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temp_directory() -> PathBuf {
        let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "lr-operation-checkpoint-{}-{id}",
            std::process::id()
        ))
    }

    fn checkpoint() -> OperationCheckpoint {
        OperationCheckpoint::new(
            "pe-install",
            OperationKind::Install,
            [
                StepDefinition::new("verify", "Verify image", true),
                StepDefinition::new("format", "Format target", false),
                StepDefinition::new("apply", "Apply image", true),
            ],
            10,
        )
        .unwrap()
    }

    #[test]
    fn state_machine_enforces_order_and_completion() {
        let mut checkpoint = checkpoint();
        assert!(checkpoint.begin_step("apply", 11).is_err());
        assert_eq!(
            checkpoint.begin_step("verify", 12).unwrap(),
            StepTransition::Applied
        );
        assert_eq!(checkpoint.step("verify").unwrap().attempts, 1);
        checkpoint.complete_step("verify", 13).unwrap();
        checkpoint.begin_step("format", 14).unwrap();
        checkpoint.complete_step("format", 15).unwrap();
        checkpoint.begin_step("apply", 16).unwrap();
        checkpoint.complete_step("apply", 17).unwrap();
        checkpoint.mark_succeeded(18).unwrap();
        assert_eq!(checkpoint.status, OperationStatus::Succeeded);
    }

    #[test]
    fn interruption_allows_only_idempotent_step_retry() {
        let mut safe = checkpoint();
        safe.begin_step("verify", 11).unwrap();
        safe.mark_interrupted(12);
        assert_eq!(safe.step("verify").unwrap().status, StepStatus::Interrupted);
        assert_eq!(
            safe.begin_step("verify", 13).unwrap(),
            StepTransition::Applied
        );
        assert_eq!(safe.step("verify").unwrap().attempts, 2);

        let mut unsafe_checkpoint = checkpoint();
        unsafe_checkpoint.skip_step("verify", 11).unwrap();
        unsafe_checkpoint.begin_step("format", 12).unwrap();
        unsafe_checkpoint.mark_interrupted(13);
        let error = unsafe_checkpoint.begin_step("format", 14).unwrap_err();
        assert!(error.message.contains("manual restart confirmation"));
    }

    #[test]
    fn atomic_store_round_trips_and_replaces_existing_checkpoint() {
        let directory = temp_directory();
        let store = CheckpointStore::new(directory.join("state.json"));
        let mut expected = checkpoint();
        store.save(&expected).unwrap();
        assert_eq!(store.load().unwrap(), Some(expected.clone()));

        expected.begin_step("verify", 20).unwrap();
        store.save(&expected).unwrap();
        assert_eq!(store.load().unwrap(), Some(expected));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);

        store.remove().unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn journal_observes_omitted_steps_and_commits_transactionally() {
        let directory = temp_directory();
        let store = CheckpointStore::new(directory.join("state.json"));
        let mut journal = OperationJournal::create(store.clone(), checkpoint()).unwrap();

        journal.observe_step("format", 20).unwrap();
        assert_eq!(
            journal.checkpoint().step("verify").unwrap().status,
            StepStatus::Skipped
        );
        assert_eq!(store.load().unwrap().unwrap(), journal.checkpoint().clone());
        journal.observe_step("apply", 21).unwrap();
        journal.complete(22).unwrap();
        assert_eq!(journal.checkpoint().status, OperationStatus::Succeeded);

        journal.remove().unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rejects_malformed_or_inconsistent_checkpoints() {
        let directory = temp_directory();
        fs::create_dir(&directory).unwrap();
        let path = directory.join("state.json");
        fs::write(&path, br#"{"schema_version":1}"#).unwrap();
        let error = CheckpointStore::new(&path).load().unwrap_err();
        assert_eq!(error.kind, super::super::OperationErrorKind::Validation);

        let mut invalid = checkpoint();
        invalid.status = OperationStatus::Succeeded;
        assert!(invalid.validate().is_err());

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
