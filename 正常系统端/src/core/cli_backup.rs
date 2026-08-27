//! Normal-Windows CLI adapter for the native backup planner/executor.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::cli_config::{BackupSpec, CliBackupExecutionMode, CliBackupOutputPolicy};
use super::disk::DiskManager;
use super::install_config::BackupConfig;
use super::native_backup_controller::{
    decide_launch_mode, plan_backup_launch, BackupLaunchMode, BackupLaunchPlan,
};
use super::native_backup_executor::{execute_backup, BackupWorkerMessage};

pub struct PreparedBackup {
    plan: BackupLaunchPlan,
    requested_execution_mode: CliBackupExecutionMode,
    requested_output_policy: CliBackupOutputPolicy,
    auto_reboot: bool,
    wim_engine: u8,
    source_identity: lr_core::windows_storage::StableVolumeIdentity,
    destination_identity: lr_core::windows_storage::StableVolumeIdentity,
    destination_parent_pins: lr_core::scoped_temp_file::PinnedDirectoryAncestors,
    destination_path: PathBuf,
    destination_base: Option<lr_core::backup_atomic_publish::FileExpectation>,
    source_bitlocker: super::bitlocker::VolumeStatus,
    destination_bitlocker: super::bitlocker::VolumeStatus,
    pe_summary: Value,
}

pub fn plan_backup(spec: &BackupSpec) -> Result<PreparedBackup> {
    let partitions =
        DiskManager::get_partitions().context("failed to refresh partition inventory")?;
    let source = partitions
        .iter()
        .find(|partition| {
            partition
                .letter
                .eq_ignore_ascii_case(&spec.source_partition)
        })
        .ok_or_else(|| {
            anyhow!(
                "source partition {} is absent from the fresh inventory",
                spec.source_partition
            )
        })?;
    let source_letter = spec
        .source_partition
        .chars()
        .next()
        .ok_or_else(|| anyhow!("source partition has no drive letter"))?;
    let source_identity = source.stable_identity.ok_or_else(|| {
        anyhow!(
            "source partition {} has no stable volume identity",
            spec.source_partition
        )
    })?;
    let fresh_source = lr_core::windows_storage::stable_volume_identity(source_letter)
        .map_err(|error| anyhow!("failed to bind source identity: {error}"))?;
    if !lr_core::windows_storage::same_stable_volume_identity(source_identity, fresh_source) {
        return Err(anyhow!(
            "source volume identity changed during backup planning"
        ));
    }

    let destination_path = PathBuf::from(&spec.save_path);
    let destination_letter = absolute_local_drive_letter(&destination_path)?;
    let destination_parent = destination_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("backup destination has no parent directory"))?;
    let parent_metadata = std::fs::symlink_metadata(destination_parent).with_context(|| {
        format!(
            "inspect backup destination directory {}",
            destination_parent.display()
        )
    })?;
    if !parent_metadata.is_dir() {
        return Err(anyhow!("backup destination parent is not a directory"));
    }
    let destination_parent_pins =
        lr_core::scoped_temp_file::pin_existing_directory_ancestors(destination_parent)
            .context("failed to pin backup destination path")?;
    destination_parent_pins
        .verify_unchanged()
        .context("backup destination path changed during planning")?;
    let destination_base = bind_destination_policy(&destination_path, spec.output_policy)?;
    let destination_identity = lr_core::windows_storage::stable_volume_identity(destination_letter)
        .map_err(|error| anyhow!("failed to bind destination volume identity: {error}"))?;
    if lr_core::windows_storage::same_stable_volume_identity(source_identity, destination_identity)
    {
        return Err(anyhow!(
            "backup destination must be on a different stable volume than the source"
        ));
    }
    let destination = partitions
        .iter()
        .find(|partition| partition.letter.starts_with(destination_letter))
        .ok_or_else(|| anyhow!("backup destination volume is absent from fresh inventory"))?;

    let is_pe_environment = DiskManager::is_pe_environment();
    let mode = resolve_requested_mode(
        spec.execution_mode,
        is_pe_environment,
        source.is_system_partition,
    )?;
    validate_bitlocker_for_mode(mode, source.bitlocker_status, destination.bitlocker_status)?;
    if spec.auto_reboot && mode != BackupLaunchMode::ViaPe {
        return Err(anyhow!(
            "auto_reboot requested, but the effective backup mode is direct"
        ));
    }
    let verified_pe = if mode == BackupLaunchMode::ViaPe {
        Some(
            first_cached_pe_verified_summary()?
                .ok_or_else(|| anyhow!("ViaPE backup requires a verified cached PE entry"))?,
        )
    } else {
        None
    };

    let config = BackupConfig {
        save_path: spec.save_path.clone(),
        name: spec.name.clone(),
        description: spec.description.clone(),
        source_partition: spec.source_partition.clone(),
        incremental: spec.output_policy == CliBackupOutputPolicy::Append,
        format: spec.format.as_u8(),
        swm_split_size: 4096,
        wim_engine: lr_core::active_engine().as_u8(),
        handoff: None,
    };
    let selected_pe = verified_pe.as_ref().map(|(pe, _)| pe);
    let planner_source_requires_pe =
        source.is_system_partition || spec.execution_mode == CliBackupExecutionMode::ViaPe;
    let plan = plan_backup_launch(
        &config,
        is_pe_environment,
        planner_source_requires_pe,
        selected_pe,
    )
    .map_err(|error| anyhow!("backup preflight rejected the request: {error}"))?;
    if plan.preview.mode != mode {
        return Err(anyhow!(
            "backup planner mode changed after CLI mode selection: selected {mode:?}, planned {:?}",
            plan.preview.mode
        ));
    }
    let pe_summary = verified_pe
        .map(|(_, summary)| summary)
        .unwrap_or(Value::Null);
    Ok(PreparedBackup {
        plan,
        requested_execution_mode: spec.execution_mode,
        requested_output_policy: spec.output_policy,
        auto_reboot: spec.auto_reboot,
        wim_engine: config.wim_engine,
        source_identity,
        destination_identity,
        destination_parent_pins,
        destination_path,
        destination_base,
        source_bitlocker: source.bitlocker_status,
        destination_bitlocker: destination.bitlocker_status,
        pe_summary,
    })
}
pub fn backup_plan_json(prepared: &PreparedBackup) -> Value {
    let plan = &prepared.plan;
    let output_policy = prepared.requested_output_policy.as_str();
    let destination_existed = prepared.destination_base.is_some();
    json!({
        "mode": backup_launch_mode_name(plan.preview.mode),
        "requested_execution_mode": prepared.requested_execution_mode.as_str(),
        "requested_output_policy": prepared.requested_output_policy.as_str(),
        "source_partition": plan.preview.source_partition,
        "destination": plan.preview.destination,
        "format": plan.preview.format,
        "incremental": prepared.requested_output_policy == CliBackupOutputPolicy::Append,
        "destination_policy": output_policy,
        "destination_existed": destination_existed,
        "source_stable_identity_digest": stable_digest(prepared.source_identity),
        "destination_stable_identity_digest": stable_digest(prepared.destination_identity),
        "source_bitlocker": format!("{:?}", prepared.source_bitlocker).to_ascii_lowercase(),
        "destination_bitlocker": format!("{:?}", prepared.destination_bitlocker).to_ascii_lowercase(),
        "requires_pe_preparation": plan.preview.requires_pe_preparation,
        "pe_display_name": plan.preview.pe_display_name,
        "pe": prepared.pe_summary.clone(),
        "effective_config": {
            "source_partition": plan.preview.source_partition,
            "save_path": plan.preview.destination,
            "format": plan.preview.format,
            "execution_mode": backup_launch_mode_name(plan.preview.mode),
            "output_policy": output_policy,
            "auto_reboot": prepared.auto_reboot,
            "wim_engine": prepared.wim_engine,
            "wim_engine_name": lr_core::WimEngine::from_u8(prepared.wim_engine).name(),
        },
        "warnings": [],
    })
}

fn resolve_requested_mode(
    requested: CliBackupExecutionMode,
    is_pe_environment: bool,
    source_is_system_partition: bool,
) -> Result<BackupLaunchMode> {
    let automatic = decide_launch_mode(is_pe_environment, source_is_system_partition);
    match requested {
        CliBackupExecutionMode::Auto => Ok(automatic),
        CliBackupExecutionMode::Direct if automatic == BackupLaunchMode::Direct => Ok(automatic),
        CliBackupExecutionMode::Direct => Err(anyhow!(
            "execution_mode=direct cannot capture the live system partition; use auto or via_pe"
        )),
        CliBackupExecutionMode::ViaPe if is_pe_environment => Err(anyhow!(
            "execution_mode=via_pe cannot schedule another PE handoff while already running in PE"
        )),
        CliBackupExecutionMode::ViaPe => Ok(BackupLaunchMode::ViaPe),
    }
}

fn validate_bitlocker_for_mode(
    mode: BackupLaunchMode,
    source: super::bitlocker::VolumeStatus,
    destination: super::bitlocker::VolumeStatus,
) -> Result<()> {
    use super::bitlocker::VolumeStatus;
    let direct_safe = |status| {
        matches!(
            status,
            VolumeStatus::NotEncrypted | VolumeStatus::EncryptedUnlocked
        )
    };
    match mode {
        BackupLaunchMode::Direct if direct_safe(source) && direct_safe(destination) => Ok(()),
        BackupLaunchMode::Direct => Err(anyhow!(
            "direct backup BitLocker state is not safe (source={source:?}, destination={destination:?})"
        )),
        BackupLaunchMode::ViaPe
            if source == VolumeStatus::NotEncrypted
                && destination == VolumeStatus::NotEncrypted =>
        {
            Ok(())
        }
        BackupLaunchMode::ViaPe => Err(anyhow!(
            "ViaPE backup requires unencrypted source and destination volumes (source={source:?}, destination={destination:?}); recovery secrets are never handed off"
        )),
    }
}
const fn backup_launch_mode_name(value: BackupLaunchMode) -> &'static str {
    match value {
        BackupLaunchMode::Direct => "direct",
        BackupLaunchMode::ViaPe => "via_pe",
    }
}

/// Runs the existing backup executor. The caller must enforce explicit `--yes`.
/// Runs the existing backup executor. The caller must enforce explicit `--yes`.
pub fn run_backup(prepared: PreparedBackup) -> Result<Value> {
    prepared
        .destination_parent_pins
        .verify_unchanged()
        .context("backup destination path changed before execution")?;
    let source_letter = prepared
        .plan
        .preview
        .source_partition
        .chars()
        .next()
        .ok_or_else(|| anyhow!("planned source partition has no drive letter"))?;
    let destination_letter = absolute_local_drive_letter(&prepared.destination_path)?;
    let source_identity = lr_core::windows_storage::stable_volume_identity(source_letter)
        .map_err(|error| anyhow!("failed to revalidate source volume identity: {error}"))?;
    let destination_identity = lr_core::windows_storage::stable_volume_identity(destination_letter)
        .map_err(|error| anyhow!("failed to revalidate destination volume identity: {error}"))?;
    if !lr_core::windows_storage::same_stable_volume_identity(
        prepared.source_identity,
        source_identity,
    ) || !lr_core::windows_storage::same_stable_volume_identity(
        prepared.destination_identity,
        destination_identity,
    ) {
        return Err(anyhow!(
            "backup source or destination stable volume identity changed before execution"
        ));
    }
    match prepared.destination_base {
        None => match std::fs::symlink_metadata(&prepared.destination_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(anyhow!("inspect backup destination: {error}")),
            Ok(_) => return Err(anyhow!("backup destination appeared after confirmation")),
        },
        Some(expected) => {
            let current =
                lr_core::backup_atomic_publish::inspect_existing_file(&prepared.destination_path)
                    .context("revalidate CLI existing backup base identity")?;
            if current != expected {
                return Err(anyhow!(
                    "existing backup changed after confirmation; refusing replace/append"
                ));
            }
        }
    }
    let inventory = DiskManager::get_partitions()
        .context("failed to refresh backup volume inventory before execution")?;
    let source = inventory
        .iter()
        .find(|partition| {
            partition
                .letter
                .eq_ignore_ascii_case(&prepared.plan.preview.source_partition)
        })
        .ok_or_else(|| anyhow!("source volume disappeared before execution"))?;
    let destination = inventory
        .iter()
        .find(|partition| partition.letter.starts_with(destination_letter))
        .ok_or_else(|| anyhow!("destination volume disappeared before execution"))?;
    validate_bitlocker_for_mode(
        prepared.plan.preview.mode,
        source.bitlocker_status,
        destination.bitlocker_status,
    )?;

    let auto_reboot = prepared.auto_reboot;
    let plan = prepared.plan;
    let planned_mode = plan.preview.mode;
    let execution = execute_backup(plan.intent)
        .map_err(|error| anyhow!("native backup executor failed to start: {error}"))?;
    let mut event_count = 0usize;
    let mut last_emitted_progress = None::<u8>;
    for event in execution.messages {
        event_count = event_count.saturating_add(1);
        let progress = match &event {
            BackupWorkerMessage::Started { mode } => {
                json!({"event":"started","mode":format!("{mode:?}")})
            }
            BackupWorkerMessage::Progress { percentage, .. } => {
                json!({"event":"progress","percentage":percentage})
            }
            BackupWorkerMessage::CancellationRequested {
                operation_may_still_be_running,
            } => {
                json!({"event":"cancellation_requested","operation_may_still_be_running":operation_may_still_be_running})
            }
            BackupWorkerMessage::PeCommitStarted => json!({"event":"pe_commit_started"}),
            BackupWorkerMessage::Completed { mode } => {
                json!({"event":"completed","mode":format!("{mode:?}")})
            }
            BackupWorkerMessage::Cancelled { output_may_exist } => {
                json!({"event":"cancelled","output_may_exist":output_may_exist})
            }
            BackupWorkerMessage::Failed { mode, .. } => {
                json!({"event":"failed","mode":format!("{mode:?}")})
            }
        };
        let should_emit = match &event {
            BackupWorkerMessage::Progress { percentage, .. } => {
                let changed = last_emitted_progress != Some(*percentage);
                if changed {
                    last_emitted_progress = Some(*percentage);
                }
                changed
            }
            _ => true,
        };
        if should_emit {
            super::cli::emit_progress(progress);
        }
        match event {
            BackupWorkerMessage::Completed { mode } => {
                if mode != planned_mode {
                    return Err(anyhow!(
                        "backup executor mode changed after planning: planned {planned_mode:?}, completed {mode:?}"
                    ));
                }
                let (outcome, backup_not_yet_created) = match mode {
                    BackupLaunchMode::Direct => ("backup_completed", false),
                    BackupLaunchMode::ViaPe => ("ready_to_reboot_into_pe", true),
                };
                let mut restart_scheduled = false;
                let mut warnings = Vec::<Value>::new();
                if auto_reboot {
                    match lr_core::windows_shutdown::schedule_restart(
                        5,
                        "LetRecovery backup preparation completed; Windows will restart into PE.",
                    ) {
                        Ok(()) => restart_scheduled = true,
                        Err(error) => warnings.push(json!({
                            "code": "restart_not_scheduled",
                            "message": format!(
                                "backup handoff was already committed, but the requested restart could not be scheduled: {error}"
                            ),
                        })),
                    }
                }
                return Ok(json!({
                    "outcome": outcome,
                    "backup_not_yet_created": backup_not_yet_created,
                    "auto_reboot_requested": auto_reboot,
                    "restart_scheduled": restart_scheduled,
                    "warnings": warnings,
                    "event_count": event_count,
                }));
            }
            BackupWorkerMessage::Cancelled { .. } => return Err(anyhow!("backup was cancelled")),
            BackupWorkerMessage::Failed { error, .. } => {
                return Err(anyhow!("backup failed: {error}"))
            }
            _ => {}
        }
    }
    Err(anyhow!("backup worker ended without a terminal result"))
}

fn first_cached_pe_verified_summary() -> Result<Option<(crate::download::config::OnlinePE, Value)>>
{
    let Some(entries) = crate::download::config::PeCache::load_strict()
        .context("failed to load PE cache catalog")?
    else {
        return Ok(None);
    };
    let mut selected = None;
    for pe in entries {
        let status = super::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        )
        .with_context(|| format!("failed to verify cached PE entry {}", pe.filename))?;
        if selected.is_none() {
            if let lr_core::cached_artifact::CachedArtifactStatus::Ready { path, .. } = status {
                let summary = json!({
                    "display_name": pe.display_name.clone(),
                    "filename": pe.filename.clone(),
                    "status": "ready",
                    "path": path,
                    "local_wim_customization_allowed": true,
                });
                selected = Some((pe, summary));
            }
        }
    }
    Ok(selected)
}

fn revalidate_destination_identity(
    path: &Path,
    expected: lr_core::windows_storage::StableVolumeIdentity,
    stage: &str,
) -> Result<()> {
    let letter = absolute_local_drive_letter(path)?;
    let current = lr_core::windows_storage::stable_volume_identity(letter)
        .map_err(|error| anyhow!("destination identity query failed {stage}: {error}"))?;
    if !lr_core::windows_storage::same_stable_volume_identity(expected, current) {
        return Err(anyhow!("backup destination identity changed {stage}"));
    }
    Ok(())
}

fn stable_digest(identity: lr_core::windows_storage::StableVolumeIdentity) -> String {
    let digest = blake3::hash(format!("{identity:?}").as_bytes()).to_hex();
    digest[..16].to_owned()
}

fn bind_destination_policy(
    path: &Path,
    policy: CliBackupOutputPolicy,
) -> Result<Option<lr_core::backup_atomic_publish::FileExpectation>> {
    match policy {
        CliBackupOutputPolicy::Create => match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(anyhow!("inspect backup destination: {error}")),
            Ok(_) => Err(anyhow!(
                "backup create policy requires an absent destination"
            )),
        },
        CliBackupOutputPolicy::Replace | CliBackupOutputPolicy::Append => Ok(Some(
            lr_core::backup_atomic_publish::inspect_existing_file(path)
                .context("bind CLI existing backup base identity")?,
        )),
    }
}

fn absolute_local_drive_letter(path: &Path) -> Result<char> {
    let text = path.as_os_str().to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return Err(anyhow!(
            "CLI backup destination must be an absolute local drive path; UNC and mapped-path identity cannot be proven safely"
        ));
    }
    Ok((bytes[0] as char).to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn cli_destination_policies_bind_create_replace_and_append_truthfully() {
        let root = std::env::temp_dir().join(format!(
            "lr-cli-backup-policy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let target = root.join("backup.wim");
        assert_eq!(
            bind_destination_policy(&target, CliBackupOutputPolicy::Create).unwrap(),
            None
        );
        assert!(bind_destination_policy(&target, CliBackupOutputPolicy::Replace).is_err());

        let mut file = std::fs::File::create(&target).unwrap();
        file.write_all(b"existing-image").unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert!(bind_destination_policy(&target, CliBackupOutputPolicy::Create).is_err());
        let expected =
            lr_core::backup_atomic_publish::FileExpectation::from_bytes(b"existing-image");
        assert_eq!(
            bind_destination_policy(&target, CliBackupOutputPolicy::Replace).unwrap(),
            Some(expected)
        );
        assert_eq!(
            bind_destination_policy(&target, CliBackupOutputPolicy::Append).unwrap(),
            Some(expected)
        );
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn cli_backup_mode_selection_honors_explicit_direct_and_via_pe() {
        assert_eq!(
            resolve_requested_mode(CliBackupExecutionMode::Auto, false, true).unwrap(),
            BackupLaunchMode::ViaPe
        );
        assert_eq!(
            resolve_requested_mode(CliBackupExecutionMode::Auto, false, false).unwrap(),
            BackupLaunchMode::Direct
        );
        assert_eq!(
            resolve_requested_mode(CliBackupExecutionMode::Direct, false, false).unwrap(),
            BackupLaunchMode::Direct
        );
        assert!(resolve_requested_mode(CliBackupExecutionMode::Direct, false, true).is_err());
        assert_eq!(
            resolve_requested_mode(CliBackupExecutionMode::ViaPe, false, true).unwrap(),
            BackupLaunchMode::ViaPe
        );
        assert_eq!(
            resolve_requested_mode(CliBackupExecutionMode::ViaPe, false, false).unwrap(),
            BackupLaunchMode::ViaPe
        );
        assert!(resolve_requested_mode(CliBackupExecutionMode::ViaPe, true, false).is_err());
    }

    #[test]
    fn cli_backup_bitlocker_policy_matches_direct_and_via_pe_execution() {
        use super::super::bitlocker::VolumeStatus;

        assert!(validate_bitlocker_for_mode(
            BackupLaunchMode::Direct,
            VolumeStatus::EncryptedUnlocked,
            VolumeStatus::NotEncrypted,
        )
        .is_ok());
        assert!(validate_bitlocker_for_mode(
            BackupLaunchMode::ViaPe,
            VolumeStatus::EncryptedUnlocked,
            VolumeStatus::NotEncrypted,
        )
        .is_err());
        assert!(validate_bitlocker_for_mode(
            BackupLaunchMode::ViaPe,
            VolumeStatus::NotEncrypted,
            VolumeStatus::NotEncrypted,
        )
        .is_ok());
    }
}
