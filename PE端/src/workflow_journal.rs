//! Durable PE workflow checkpoints and sanitized support bundles.
//!
//! This observer never decides whether a destructive step should run. A prior
//! interrupted task is recorded for diagnostics, then the existing workflow
//! restarts conservatively from its normal entry point.

use std::path::{Path, PathBuf};

use lr_core::operation::{
    unix_time_millis, CheckpointStore, OperationCheckpoint, OperationError, OperationErrorKind,
    OperationJournal, OperationKind, OperationStatus, StepDefinition, SupportBundleBuilder,
};

use crate::core::config::{ConfigFileManager, OperationType};
use crate::ui::progress::{BackupStep, InstallStep};

const CHECKPOINT_FILE: &str = "LetRecovery.operation.json";
const SUPPORT_FILE: &str = "LetRecovery-support.json";

pub(crate) struct PeWorkflowJournal {
    journal: OperationJournal,
    support_path: PathBuf,
    data_partition: String,
}

impl PeWorkflowJournal {
    pub(crate) fn create(operation_type: OperationType) -> Result<Option<Self>, OperationError> {
        let Some(data_partition) = ConfigFileManager::find_data_partition() else {
            return Ok(None);
        };
        let root = partition_root(&data_partition)?;
        let store = CheckpointStore::new(root.join(CHECKPOINT_FILE));
        let support_path = root.join(SUPPORT_FILE);
        let now = unix_time_millis();

        if let Some(mut previous) = OperationJournal::open(store.clone())? {
            if !matches!(
                previous.checkpoint().status,
                OperationStatus::Succeeded | OperationStatus::Cancelled
            ) {
                previous.mark_interrupted(now)?;
                write_support_bundle(
                    previous.checkpoint(),
                    &support_path,
                    &data_partition,
                    "previous_interrupted",
                )?;
            }
            previous.remove()?;
        }

        let (kind, steps, id_prefix) = operation_definition(operation_type);
        let checkpoint =
            OperationCheckpoint::new(format!("pe-{id_prefix}-{now}"), kind, steps, now)?;
        let mut journal = OperationJournal::create(store, checkpoint)?;
        if operation_type == OperationType::Expand {
            journal.observe_step("expand", now)?;
        }

        Ok(Some(Self {
            journal,
            support_path,
            data_partition,
        }))
    }

    pub(crate) fn observe_install_step(&mut self, step: InstallStep) -> Result<(), OperationError> {
        self.journal
            .observe_step(install_step_id(step), unix_time_millis())?;
        Ok(())
    }

    pub(crate) fn observe_backup_step(&mut self, step: BackupStep) -> Result<(), OperationError> {
        self.journal
            .observe_step(backup_step_id(step), unix_time_millis())?;
        Ok(())
    }

    pub(crate) fn fail(&mut self, message: &str) -> Result<(), OperationError> {
        let error =
            OperationError::new(OperationErrorKind::Unknown, None::<String>, message, false);
        self.journal.fail_current(error, unix_time_millis())?;
        write_support_bundle(
            self.journal.checkpoint(),
            &self.support_path,
            &self.data_partition,
            "workflow_failure",
        )
    }

    pub(crate) fn complete(&mut self) -> Result<(), OperationError> {
        self.journal.complete(unix_time_millis())?;
        self.journal.remove()
    }
}

fn operation_definition(
    operation_type: OperationType,
) -> (OperationKind, Vec<StepDefinition>, &'static str) {
    match operation_type {
        OperationType::Install => (
            OperationKind::Install,
            InstallStep::all()
                .into_iter()
                .map(|step| {
                    StepDefinition::new(
                        install_step_id(step),
                        step.name(),
                        install_step_is_idempotent(step),
                    )
                })
                .collect(),
            "install",
        ),
        OperationType::Backup => (
            OperationKind::Backup,
            BackupStep::all()
                .into_iter()
                .map(|step| {
                    StepDefinition::new(
                        backup_step_id(step),
                        step.name(),
                        backup_step_is_idempotent(step),
                    )
                })
                .collect(),
            "backup",
        ),
        OperationType::Expand => (
            OperationKind::Expand,
            vec![StepDefinition::new("expand", "无损扩大分区", false)],
            "expand",
        ),
    }
}

fn install_step_id(step: InstallStep) -> &'static str {
    match step {
        InstallStep::VerifyImage => "verify_image",
        InstallStep::FormatPartition => "format_partition",
        InstallStep::ApplyImage => "apply_image",
        InstallStep::ImportDrivers => "import_drivers",
        InstallStep::InstallCabPackages => "install_cab_packages",
        InstallStep::RepairBoot => "repair_boot",
        InstallStep::ApplyAdvancedOptions => "apply_advanced_options",
        InstallStep::GenerateUnattend => "generate_unattend",
        InstallStep::Cleanup => "cleanup",
        InstallStep::Complete => "complete",
    }
}

fn backup_step_id(step: BackupStep) -> &'static str {
    match step {
        BackupStep::ReadConfig => "read_config",
        BackupStep::CaptureImage => "capture_image",
        BackupStep::VerifyBackup => "verify_backup",
        BackupStep::RepairBoot => "repair_boot",
        BackupStep::Cleanup => "cleanup",
        BackupStep::Complete => "complete",
    }
}

fn install_step_is_idempotent(step: InstallStep) -> bool {
    matches!(
        step,
        InstallStep::VerifyImage
            | InstallStep::GenerateUnattend
            | InstallStep::Cleanup
            | InstallStep::Complete
    )
}

fn backup_step_is_idempotent(step: BackupStep) -> bool {
    matches!(
        step,
        BackupStep::ReadConfig
            | BackupStep::VerifyBackup
            | BackupStep::RepairBoot
            | BackupStep::Cleanup
            | BackupStep::Complete
    )
}

fn partition_root(partition: &str) -> Result<PathBuf, OperationError> {
    let trimmed = partition.trim().trim_end_matches(['\\', '/']);
    let bytes = trimmed.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(OperationError::validation(
            "PE workflow data partition must be a drive letter",
        ));
    }
    Ok(PathBuf::from(format!("{}\\", trimmed.to_ascii_uppercase())))
}

fn write_support_bundle(
    checkpoint: &OperationCheckpoint,
    destination: &Path,
    data_partition: &str,
    reason: &str,
) -> Result<(), OperationError> {
    let mut builder = SupportBundleBuilder::new(
        "LetRecovery",
        env!("CARGO_PKG_VERSION"),
        "pe",
        unix_time_millis(),
    )?;
    builder.add_environment("reason", reason)?;
    builder.add_environment("data_partition", data_partition)?;
    builder.set_operation(checkpoint);

    if let Some(log_path) = pe_log_path() {
        if log_path.is_file() {
            builder.add_text_file("runtime_log", &log_path)?;
        }
    }
    builder.build().write_json(destination)
}

fn pe_log_path() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|path| {
        path.parent()
            .map(|directory| directory.join("LetRecoveryPE.log"))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_ui_step_has_one_stable_checkpoint_id() {
        for operation in [OperationType::Install, OperationType::Backup] {
            let (_, steps, _) = operation_definition(operation);
            let unique: HashSet<&str> = steps.iter().map(|step| step.id.as_str()).collect();
            assert_eq!(unique.len(), steps.len());
            assert!(steps.iter().all(|step| !step.name.trim().is_empty()));
        }
    }

    #[test]
    fn drive_root_validation_rejects_paths_and_metacharacters() {
        assert_eq!(partition_root("d:").unwrap(), PathBuf::from("D:\\"));
        for invalid in ["", "C:\\Windows", "1:", "C:&", "C: && format D:"] {
            assert!(partition_root(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn destructive_steps_are_not_marked_idempotent() {
        assert!(!install_step_is_idempotent(InstallStep::FormatPartition));
        assert!(!install_step_is_idempotent(InstallStep::ApplyImage));
        assert!(!backup_step_is_idempotent(BackupStep::CaptureImage));
    }
}
