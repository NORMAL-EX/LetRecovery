//! Normal-Windows CLI adapter for the native installation controller/executor.
//!
//! No PE handoff or `InstallConfig` is assembled here.  Planning uses a fresh partition and
//! image inventory; execution delegates every phase to `ProductionInstallBackend`.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

use super::bitlocker::VolumeStatus;
use super::cli_config::{
    AdvancedSpec, CliBootMode, CliBootPcaMode, CliDriverAction, CliInstallMode, InstallSpec,
};
use super::disk::{DiskManager, Partition};
use super::dism::Dism;
use super::native_install_backend::ProductionInstallBackend;
use super::native_install_controller::{
    InstallTarget, NativeInstallState, SelectedImageMetadata, StartInstallIntent,
};
use super::native_install_executor::{
    BitLockerRequirement, InstallExecutionContext, InstallExecutionEvent, InstallExecutionOutcome,
    NativeInstallExecutor, StableTargetIdentity,
};
use super::ui_state::{AdvancedOptionsData, BootModeSelection, DriverAction, InstallPrefs};

pub struct PreparedInstall {
    pub intent: StartInstallIntent,
    pub context: InstallExecutionContext,
    pub phases: Vec<super::native_install_executor::InstallExecutionPhase>,
    image_summary: Value,
    target_summary: Value,
    pe_summary: Value,
    planning_warnings: Vec<Value>,
}

#[derive(Clone)]
struct PeBinding {
    index: usize,
}

pub fn plan_install(spec: &InstallSpec) -> Result<PreparedInstall> {
    require_regular_file(&spec.image_path, "image_path")?;
    if !spec.image_backing_path.is_empty() {
        require_regular_file(&spec.image_backing_path, "image_backing_path")?;
    }
    if !spec.custom_unattend_path.is_empty() {
        require_regular_file(&spec.custom_unattend_path, "custom_unattend_path")?;
    }

    let partitions = DiskManager::get_install_partitions()
        .context("failed to refresh install-target inventory")?;
    let partition = partitions
        .iter()
        .find(|candidate| {
            candidate
                .letter
                .eq_ignore_ascii_case(&spec.target_partition)
        })
        .ok_or_else(|| {
            anyhow!(
                "target partition {} is absent from the fresh inventory",
                spec.target_partition
            )
        })?;
    let disk_bus_type = partition.disk_number.and_then(|disk| {
        lr_core::windows_storage::disk_bus_type(disk)
            .map_err(|error| {
                log::warn!("[CLI INSTALL] disk bus query failed for disk {disk}: {error}");
                error
            })
            .ok()
    });
    let (selected_image, image_summary, image_requirement) = selected_image_metadata(spec)?;
    let (mut prefs, planning_warnings) = prefs_from_spec(spec)?;
    // Installation scope is destructive authority, not a cosmetic GUI preference. Always replace
    // any inherited stale plan with the explicit versioned CLI request built from fresh inventory.
    prefs.custom_install_plan = custom_install_plan_from_spec(spec, partition, image_requirement)?;
    let verified_pe = if partition.is_system_partition && !DiskManager::is_pe_environment() {
        first_cached_pe_verified_summary()?
    } else {
        None
    };
    let pe_available = verified_pe.is_some();
    let state = NativeInstallState {
        image_path: spec.image_path.clone(),
        image_backing_path: spec.image_backing_path.clone(),
        image_ready: true,
        selected_image,
        xp_i386_source: None,
        target: Some(target_from_partition(partition, disk_bus_type)),
        is_pe_environment: DiskManager::is_pe_environment(),
        pe_available,
        custom_unattend_path: spec.custom_unattend_path.clone(),
        custom_unattend_error: None,
        partition_refresh_pending: false,
        partition_refresh_error: None,
        pca_detection_pending: false,
        pca_selection_error: None,
        advanced_options_enabled: true,
        prefs,
    };
    let mut intent = state
        .start_intent()
        .map_err(|error| anyhow!("install preflight rejected the request: {error}"))?;
    // This policy is intentionally absent from persistent GUI preferences. Only the explicit,
    // versioned public CLI document may arm automatic terminal power-off.
    intent.options.automation_shutdown_on_terminal = spec.automation_shutdown_on_terminal;
    if let Some((binding, _)) = &verified_pe {
        intent.pe_index = Some(binding.index);
    }
    let context = execution_context(&intent, partition);
    let phases = NativeInstallExecutor::build_plan(&intent, &context)
        .map_err(|error| anyhow!("install execution plan rejected the request: {error}"))?;
    Ok(PreparedInstall {
        intent,
        context,
        phases,
        image_summary,
        target_summary: json!({
            "stable_identity_digest": stable_digest(partition.stable_identity.expect("controller accepted stable identity")),
            "bitlocker": format!("{:?}", partition.bitlocker_status).to_ascii_lowercase(),
            "has_windows": partition.has_windows,
            "is_current_system": partition.is_system_partition,
            "partition_style": format!("{:?}", partition.partition_style).to_ascii_lowercase(),
        }),
        pe_summary: verified_pe
            .as_ref()
            .map(|(_, summary)| summary.clone())
            .unwrap_or(Value::Null),
        planning_warnings,
    })
}

fn first_cached_pe_verified_summary() -> Result<Option<(PeBinding, Value)>> {
    let Some(entries) = crate::download::config::PeCache::load_strict()
        .context("failed to load PE cache catalog")?
    else {
        return Ok(None);
    };
    let mut selected = None;
    for (index, pe) in entries.into_iter().enumerate() {
        let status = super::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        )
        .with_context(|| format!("failed to verify cached PE entry {}", pe.filename))?;
        if selected.is_none() {
            if let lr_core::cached_artifact::CachedArtifactStatus::Ready { path, .. } = status {
                selected = Some((
                    PeBinding { index },
                    json!({
                        "display_name":pe.display_name,
                        "filename":pe.filename,
                        "status":"ready",
                        "path":path,
                        "local_wim_customization_allowed":true,
                    }),
                ));
            }
        }
    }
    Ok(selected)
}

pub fn install_plan_json(prepared: &PreparedInstall) -> Value {
    let options = &prepared.intent.options;
    json!({
        "mode": format!("{:?}", prepared.intent.mode).to_ascii_lowercase(),
        "custom_install_mode": prepared
            .intent
            .options
            .custom_install_plan
            .mode()
            .as_config_value(),
        "target_partition": prepared.intent.target_partition,
        "target": {
            "disk_number": prepared.intent.target_disk_number,
            "partition_number": prepared.intent.target_partition_number,
            "offset_bytes": prepared.intent.target_partition_offset_bytes,
            "length_bytes": prepared.intent.target_partition_size_bytes,
            "inventory": prepared.target_summary,
        },
        "image_path": prepared.intent.image_path,
        "image": prepared.image_summary,
        "volume_index": prepared.intent.volume_index,
        "format_partition": options.format_partition,
        "repair_boot": options.repair_boot,
        "auto_reboot": options.auto_reboot,
        "automation_shutdown_on_terminal": options.automation_shutdown_on_terminal,
        "driver_action": driver_action_name(options.driver_action),
        "export_drivers": options.export_drivers,
        "pe": prepared.pe_summary,
        "advanced": effective_advanced_summary(&options.advanced_options),
        "effective_config": {
            "target_partition": prepared.intent.target_partition,
            "install_mode": prepared.intent.options.custom_install_plan.mode().as_config_value(),
            "confirmed_disk_numbers": spec_confirmed_disks_for_output(&prepared.intent.options.custom_install_plan),
            "dual_boot_size_gib": dual_boot_size_gib_for_output(&prepared.intent.options.custom_install_plan),
            "image_path": prepared.intent.image_path,
            "image_backing_path": prepared.intent.image_backing_path,
            "volume_index": prepared.intent.volume_index,
            "format_partition": options.format_partition,
            "repair_boot": options.repair_boot,
            "unattended": options.unattended_install,
            "auto_reboot": options.auto_reboot,
            "automation_shutdown_on_terminal": options.automation_shutdown_on_terminal,
            "driver_action": driver_action_name(options.driver_action),
            "boot_mode": boot_mode_name(options.boot_mode),
            "boot_pca_mode": boot_pca_mode_name(options.boot_pca_mode),
            "custom_unattend_path": options.custom_unattend_path,
            "advanced": effective_advanced_summary(&options.advanced_options),
        },
        "warnings": prepared.planning_warnings.clone(),
        "phases": prepared.phases.iter().map(|phase| format!("{phase:?}")).collect::<Vec<_>>(),
    })
}

const fn driver_action_name(value: DriverAction) -> &'static str {
    match value {
        DriverAction::None => "none",
        DriverAction::SaveOnly => "save_only",
        DriverAction::AutoImport => "auto_import",
    }
}

const fn boot_mode_name(value: BootModeSelection) -> &'static str {
    match value {
        BootModeSelection::Auto => "auto",
        BootModeSelection::UEFI => "uefi",
        BootModeSelection::Legacy => "legacy",
    }
}

const fn boot_pca_mode_name(value: lr_core::boot_pca::BootPcaMode) -> &'static str {
    match value {
        lr_core::boot_pca::BootPcaMode::Auto => "auto",
        lr_core::boot_pca::BootPcaMode::Pca2011 => "pca2011",
        lr_core::boot_pca::BootPcaMode::Pca2023 => "pca2023",
    }
}

/// Runs an already-built, freshly validated plan. The caller must enforce explicit `--yes`.
pub fn run_install(prepared: PreparedInstall) -> Result<Value> {
    let mut backend = ProductionInstallBackend::new(&prepared.intent);
    let mut event_count = 0usize;
    let mut reporter = |event: InstallExecutionEvent| {
        event_count = event_count.saturating_add(1);
        let value = match event {
            InstallExecutionEvent::Started { total_phases } => {
                json!({"event":"started","total_phases":total_phases})
            }
            InstallExecutionEvent::PhaseStarted {
                index,
                total,
                phase,
                cancellable,
                overall,
            } => {
                json!({"event":"phase_started","index":index,"total":total,"phase":format!("{phase:?}"),"cancellable":cancellable,"overall_start":overall.start,"overall_end":overall.end})
            }
            InstallExecutionEvent::Progress {
                phase, percentage, ..
            } => json!({"event":"progress","phase":format!("{phase:?}"),"percentage":percentage}),
            InstallExecutionEvent::PhaseCompleted {
                index,
                total,
                phase,
                overall_end,
            } => {
                json!({"event":"phase_completed","index":index,"total":total,"phase":format!("{phase:?}"),"overall":overall_end})
            }
            InstallExecutionEvent::Completed(outcome) => {
                json!({"event":"completed","outcome":format!("{outcome:?}")})
            }
        };
        super::cli::emit_progress(value);
    };
    let never_cancel = || false;
    let outcome = NativeInstallExecutor::execute(
        &prepared.intent,
        &prepared.context,
        &mut backend,
        &mut reporter,
        &never_cancel,
    )
    .map_err(|error| anyhow!("native install executor failed: {error}"))?;
    let mut restart_scheduled = false;
    let mut warnings = Vec::<Value>::new();
    if prepared.intent.options.auto_reboot {
        let (restart_delay_seconds, force_apps_closed) =
            restart_policy(prepared.intent.options.automation_shutdown_on_terminal);
        let restart = if force_apps_closed {
            lr_core::windows_shutdown::schedule_restart_for_automation(
                restart_delay_seconds,
                "LetRecovery installation preparation completed; Windows will restart.",
            )
        } else {
            lr_core::windows_shutdown::schedule_restart(
                restart_delay_seconds,
                "LetRecovery installation preparation completed; Windows will restart.",
            )
        };
        match restart {
            Ok(()) => restart_scheduled = true,
            Err(error) => warnings.push(json!({
                "code": "restart_not_scheduled",
                "message": format!(
                    "installation was already committed, but the requested restart could not be scheduled: {error}"
                ),
            })),
        }
    }
    Ok(json!({
        "outcome": match outcome {
            InstallExecutionOutcome::DirectInstallCompleted => "direct_install_completed",
            InstallExecutionOutcome::ReadyToRebootIntoPe => "ready_to_reboot_into_pe",
        },
        "auto_reboot_requested": prepared.intent.options.auto_reboot,
        "restart_scheduled": restart_scheduled,
        "warnings": warnings,
        "event_count": event_count,
    }))
}

/// The public automation switch is deliberately opt-in and is used only for unattended disposable
/// machines. Give its controller enough time to persist the handoff and extreme-fixture witnesses,
/// then close blocking helper applications at the documented shutdown boundary. Interactive
/// installs retain the shorter, non-forcing restart so user-authored application state is never
/// discarded.
const fn restart_policy(automation_shutdown_on_terminal: bool) -> (u32, bool) {
    if automation_shutdown_on_terminal {
        (60, true)
    } else {
        (5, false)
    }
}

fn require_regular_file(path: &str, field: &str) -> Result<()> {
    super::cli_config::require_plain_regular_file(Path::new(path))
        .with_context(|| format!("{field} must identify a regular non-reparse file: {path}"))?;
    Ok(())
}

fn selected_image_metadata(
    spec: &InstallSpec,
) -> Result<(
    Option<SelectedImageMetadata>,
    Value,
    lr_core::custom_install::ImageSpaceRequirement,
)> {
    let extension = Path::new(&spec.image_path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "gho" | "ghs") {
        return Ok((
            None,
            json!({"name":null,"version":null,"build":null,"architecture":null,"type":"ghost"}),
            lr_core::custom_install::ImageSpaceRequirement::fallback(),
        ));
    }
    if !matches!(extension.as_str(), "wim" | "esd" | "swm") {
        return Err(anyhow!(
            "unsupported CLI image type .{extension}; use WIM, ESD, SWM, GHO or GHS"
        ));
    }
    let images = Dism::new()
        .get_image_info(&spec.image_path)
        .context("failed to read image metadata")?;
    let image = images
        .into_iter()
        .find(|image| image.index == spec.volume_index)
        .ok_or_else(|| {
            anyhow!(
                "volume_index {} is absent from the image",
                spec.volume_index
            )
        })?;
    if !super::dism::is_installable_image(&image) {
        return Err(anyhow!(
            "volume_index {} is not an installable Windows image",
            spec.volume_index
        ));
    }
    let summary = json!({
        "name": image.name,
        "version": match (image.major_version, image.minor_version) {
            (Some(major), Some(minor)) => Some(format!("{major}.{minor}")),
            _ => None,
        },
        "build": image.build,
        "architecture": image.architecture,
        "installation_type": image.installation_type,
    });
    let image_requirement =
        lr_core::custom_install::image_space_requirement(image.size_bytes, image.hard_link_bytes);
    Ok((
        Some(SelectedImageMetadata {
            volume_index: image.index,
            major_version: image.major_version,
            minor_version: image.minor_version,
            build: image.build,
            architecture: image.architecture,
        }),
        summary,
        image_requirement,
    ))
}

fn custom_install_plan_from_spec(
    spec: &InstallSpec,
    target: &Partition,
    image: lr_core::custom_install::ImageSpaceRequirement,
) -> Result<lr_core::custom_install::CustomInstallPlan> {
    match spec.install_mode {
        CliInstallMode::ReinstallPartition => {
            Ok(lr_core::custom_install::CustomInstallPlan::ReinstallPartition)
        }
        CliInstallMode::RepartitionAllDisks => {
            let windows_disk = target
                .disk_number
                .ok_or_else(|| anyhow!("target_partition has no current physical disk identity"))?;
            if !spec.confirmed_disk_numbers.contains(&windows_disk) {
                return Err(anyhow!(
                    "the current target_partition disk {windows_disk} is absent from confirmed_disk_numbers"
                ));
            }
            let inventory = super::custom_install_plan::capture_disk_inventory()
                .context("failed to capture current full-disk inventory")?;
            super::custom_install_plan::build_full_disk_plan(
                &inventory,
                &spec.confirmed_disk_numbers,
                windows_disk,
                image,
            )
            .context("failed to build the explicitly confirmed full-disk plan")
        }
        CliInstallMode::DualBoot => {
            let size_gib = spec
                .dual_boot_size_gib
                .ok_or_else(|| anyhow!("dual_boot_size_gib is required for dual_boot"))?;
            let requested_bytes = size_gib
                .checked_mul(lr_core::custom_install::GIB)
                .ok_or_else(|| anyhow!("dual_boot_size_gib exceeds the supported byte range"))?;
            if requested_bytes < image.windows_partition_bytes {
                return Err(anyhow!(
                    "dual_boot_size_gib provides {requested_bytes} bytes, but the selected image requires at least {} bytes",
                    image.windows_partition_bytes
                ));
            }
            let plan =
                super::custom_install_plan::build_dual_boot_request(target, requested_bytes, 0)
                    .context("failed to build the explicit dual-boot shrink request")?;
            Ok(lr_core::custom_install::CustomInstallPlan::DualBoot(plan))
        }
    }
}

fn spec_confirmed_disks_for_output(plan: &lr_core::custom_install::CustomInstallPlan) -> Vec<u32> {
    match plan {
        lr_core::custom_install::CustomInstallPlan::RepartitionAllDisks(plan) => plan
            .disks
            .iter()
            .map(|disk| disk.diagnostic_disk_number)
            .collect(),
        _ => Vec::new(),
    }
}

fn dual_boot_size_gib_for_output(plan: &lr_core::custom_install::CustomInstallPlan) -> Option<u64> {
    match plan {
        lr_core::custom_install::CustomInstallPlan::DualBoot(plan)
            if plan.target_length_bytes % lr_core::custom_install::GIB == 0 =>
        {
            Some(plan.target_length_bytes / lr_core::custom_install::GIB)
        }
        _ => None,
    }
}

fn stable_digest(identity: lr_core::windows_storage::StableVolumeIdentity) -> String {
    let digest = blake3::hash(format!("{identity:?}").as_bytes()).to_hex();
    digest[..16].to_owned()
}

fn effective_advanced_summary(value: &AdvancedOptionsData) -> Value {
    let basename = |path: &str| {
        (!path.is_empty()).then(|| {
            Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<invalid>".to_owned())
        })
    };
    json!({
        "remove_shortcut_arrow": value.remove_shortcut_arrow,
        "restore_classic_context_menu": value.restore_classic_context_menu,
        "bypass_nro": value.bypass_nro,
        "disable_windows_update": value.disable_windows_update,
        "disable_windows_defender": value.disable_windows_defender,
        "disable_reserved_storage": value.disable_reserved_storage,
        "disable_uac": value.disable_uac,
        "disable_device_encryption": value.disable_device_encryption,
        "remove_uwp_apps": value.remove_uwp_apps,
        "deploy_script": {"enabled":value.run_script_during_deploy,"file":basename(&value.deploy_script_path)},
        "first_login_script": {"enabled":value.run_script_first_login,"file":basename(&value.first_login_script_path)},
        "custom_drivers": {"enabled":value.import_custom_drivers,"directory":basename(&value.custom_drivers_path)},
        "storage_controller_drivers": value.import_storage_controller_drivers,
        "registry_file": {"enabled":value.import_registry_file,"file":basename(&value.registry_file_path)},
        "custom_files": {"enabled":value.import_custom_files,"directory":basename(&value.custom_files_path)},
        "custom_username": {"enabled":value.custom_username},
        "builtin_administrator": {
            "enabled":value.builtin_administrator.enabled,
            "account_name":value.builtin_administrator.account_name,
            "auto_logon":value.builtin_administrator.auto_logon,
            "password_present":!value.builtin_administrator.password.is_empty(),
        },
        "custom_volume_label": {"enabled":value.custom_volume_label,"value":value.volume_label},
        "win7_fix_acpi_bsod": value.win7_fix_acpi_bsod,
        "xp_inject_usb3_driver": value.xp_inject_usb3_driver,
        "xp_inject_nvme_driver": value.xp_inject_nvme_driver,
    })
}

fn target_from_partition(
    partition: &Partition,
    disk_bus_type: Option<lr_core::windows_storage::DiskBusType>,
) -> InstallTarget {
    InstallTarget {
        partition: partition.letter.clone(),
        disk_number: partition.disk_number,
        partition_number: partition.partition_number,
        disk_size_bytes: partition.disk_size_bytes,
        partition_offset_bytes: partition.partition_offset_bytes,
        partition_size_bytes: partition.partition_size_bytes,
        stable_identity: partition.stable_identity,
        disk_bus_type,
        style: partition.partition_style,
        is_current_system: partition.is_system_partition,
        has_windows: partition.has_windows,
    }
}

fn execution_context(
    intent: &StartInstallIntent,
    partition: &Partition,
) -> InstallExecutionContext {
    let bitlocker = match partition.bitlocker_status {
        VolumeStatus::EncryptedLocked => BitLockerRequirement::UnlockRequired,
        VolumeStatus::Decrypting | VolumeStatus::EncryptedUnlocked => {
            BitLockerRequirement::AwaitDecryption
        }
        _ => BitLockerRequirement::Ready,
    };
    InstallExecutionContext {
        stable_target: Some(StableTargetIdentity {
            disk_number: intent.target_disk_number,
            partition_number: intent.target_partition_number,
            disk_size_bytes: intent.target_disk_size_bytes,
            partition_offset_bytes: intent.target_partition_offset_bytes,
            partition_size_bytes: intent.target_partition_size_bytes,
            stable_volume: intent.target_stable_identity,
        }),
        bitlocker,
    }
}

fn prefs_from_spec(spec: &InstallSpec) -> Result<(InstallPrefs, Vec<Value>)> {
    let mut prefs = if spec.inherit_app_install_prefs {
        super::app_config::AppConfig::load_required_strict()
            .context("failed to inherit the required adjacent application config.json")?
            .install_prefs
    } else {
        InstallPrefs {
            format_partition: spec.format_partition,
            repair_boot: spec.repair_boot,
            unattended_install: spec.unattended,
            // DriverAction is the single source of truth. The shared controller additionally checks
            // that the selected target actually contains Windows before scheduling an export.
            export_drivers: false,
            auto_reboot: spec.auto_reboot,
            run_diskpart_scripts: false,
            boot_mode: match spec.boot_mode {
                CliBootMode::Auto => BootModeSelection::Auto,
                CliBootMode::Uefi => BootModeSelection::UEFI,
                CliBootMode::Legacy => BootModeSelection::Legacy,
            },
            boot_pca_mode: match spec.boot_pca_mode {
                CliBootPcaMode::Auto => lr_core::boot_pca::BootPcaMode::Auto,
                CliBootPcaMode::Pca2011 => lr_core::boot_pca::BootPcaMode::Pca2011,
                CliBootPcaMode::Pca2023 => lr_core::boot_pca::BootPcaMode::Pca2023,
            },
            driver_action: match spec.driver_action {
                CliDriverAction::None => DriverAction::None,
                CliDriverAction::SaveOnly => DriverAction::SaveOnly,
                CliDriverAction::AutoImport => DriverAction::AutoImport,
            },
            custom_install_plan: lr_core::custom_install::CustomInstallPlan::default(),
            advanced_options: advanced_from_spec(&spec.advanced),
        }
    };
    // Historical configs remain readable but public automation can never revive DiskPart
    // scripts or a stale destructive custom-install scope. The current live v4 catalogue is the
    // only authority for software URLs and silent commands; adjacent GUI preferences intentionally
    // do not persist those values. `plan_install` replaces this safe baseline only from the
    // explicit versioned CLI full-disk request and fresh inventory.
    prefs.run_diskpart_scripts = false;
    prefs.custom_install_plan = lr_core::custom_install::CustomInstallPlan::ReinstallPartition;
    // This destructive current-run choice is explicit in the versioned CLI document and is never
    // inherited from the adjacent long-lived GUI preferences.
    prefs.advanced_options.preserve_personal_files = spec.advanced.preserve_personal_files;
    if spec.advanced.install_vmware_tools
        && lr_core::windows_hardware::collect_machine_identity().environment
            != lr_core::windows_hardware::MachineEnvironment::Vmware
    {
        return Err(anyhow!(
            "install_vmware_tools requires a positively detected VMware guest"
        ));
    }
    let (selected_software, catalogue_warning) = resolve_preinstalled_software(
        &spec.preinstalled_software_ids,
        spec.advanced.install_vmware_tools,
    )?;
    prefs.advanced_options.preinstalled_software = selected_software;
    let warnings = catalogue_warning
        .into_iter()
        .map(|message| {
            json!({
                "code": "optional_software_catalogue_unavailable",
                "message": message,
                "requested_count": spec.preinstalled_software_ids.len(),
                "selected_count": 0,
            })
        })
        .collect();
    Ok((prefs, warnings))
}

fn resolve_preinstalled_software(
    requested_ids: &[String],
    include_vmware_tools: bool,
) -> Result<(
    Vec<lr_core::software_install::SelectedSoftwarePackage>,
    Option<String>,
)> {
    if requested_ids.is_empty() && !include_vmware_tools {
        return Ok((Vec::new(), None));
    }
    let mut remote = None;
    let mut errors = Vec::new();
    for attempt in 1..=3 {
        let candidate = crate::download::server_config::RemoteConfig::load_from_server();
        if candidate.loaded {
            remote = Some(candidate);
            break;
        }
        let detail = candidate
            .error
            .unwrap_or_else(|| "unknown catalogue error".to_owned());
        errors.push(format!("attempt {attempt}/3: {detail}"));
        if attempt < 3 {
            log::warn!(
                "[CLI INSTALL] live v4 catalogue load failed; retrying attempt={attempt}/3 detail={detail}"
            );
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }
    let Some(remote) = remote else {
        let message = optional_software_catalogue_warning(&errors);
        log::warn!("[CLI INSTALL] {message}");
        return Ok((Vec::new(), Some(message)));
    };
    let content = remote
        .soft_content
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("live v4 software catalogue is empty"))?;
    let catalogue = crate::download::config::ConfigManager::load_from_content_with_soft(
        None,
        None,
        Some(content),
    );
    Ok((
        resolve_preinstalled_software_from_catalog(
            requested_ids,
            include_vmware_tools,
            &catalogue,
        )?,
        None,
    ))
}

fn optional_software_catalogue_warning(errors: &[String]) -> String {
    format!(
        "optional preinstalled-software catalogue was unavailable after 3 attempts; core Windows installation will continue without those packages: {}",
        errors.join("; ")
    )
}

fn resolve_preinstalled_software_from_catalog(
    requested_ids: &[String],
    include_vmware_tools: bool,
    catalogue: &crate::download::config::ConfigManager,
) -> Result<Vec<lr_core::software_install::SelectedSoftwarePackage>> {
    let mut selected = Vec::with_capacity(requested_ids.len() + usize::from(include_vmware_tools));
    for requested in requested_ids {
        let mut matches = catalogue
            .software_list
            .iter()
            .filter(|software| software.id.eq_ignore_ascii_case(requested));
        let software = matches.next().ok_or_else(|| {
            anyhow!("software id {requested} is absent from the live v4 catalogue")
        })?;
        if matches.next().is_some() {
            return Err(anyhow!(
                "software id {requested} is ambiguous in the live v4 catalogue"
            ));
        }
        if software.vm_tools {
            return Err(anyhow!(
                "software id {requested} is reserved for the separate VMware Tools option"
            ));
        }
        let package =
            super::native_download_controller::NativeDownloadController::selected_package(software)
                .ok_or_else(|| anyhow!("software id {requested} has no silent install command"))?;
        selected.push(package);
    }
    if include_vmware_tools {
        let software = catalogue
            .vmware_tools_entry()
            .ok_or_else(|| anyhow!("the live v4 catalogue has no unique VMware Tools entry"))?;
        let package =
            super::native_download_controller::NativeDownloadController::selected_package(software)
                .ok_or_else(|| anyhow!("the VMware Tools entry has no silent install command"))?;
        if !selected
            .iter()
            .any(|existing| existing.id.eq_ignore_ascii_case(&package.id))
        {
            selected.push(package);
        }
    }
    lr_core::software_install::validate_selected_packages(&selected)
        .context("selected software catalogue entries are invalid")?;
    Ok(selected)
}

fn advanced_from_spec(spec: &AdvancedSpec) -> AdvancedOptionsData {
    let mut value = AdvancedOptionsData {
        preserve_personal_files: spec.preserve_personal_files,
        remove_shortcut_arrow: spec.remove_shortcut_arrow,
        restore_classic_context_menu: spec.restore_classic_context_menu,
        bypass_nro: spec.bypass_nro,
        disable_windows_update: spec.disable_windows_update,
        disable_windows_defender: spec.disable_windows_defender,
        disable_reserved_storage: spec.disable_reserved_storage,
        disable_uac: spec.disable_uac,
        disable_device_encryption: spec.disable_device_encryption,
        remove_uwp_apps: spec.remove_uwp_apps,
        install_vmware_tools: spec.install_vmware_tools,
        migrate_wifi: spec.migrate_wifi,
        wifi_ssid: spec.wifi_ssid.clone(),
        wifi_profile_xml: spec.wifi_profile_xml.clone(),
        run_script_during_deploy: !spec.deploy_script_path.is_empty(),
        deploy_script_path: spec.deploy_script_path.clone(),
        run_script_first_login: !spec.first_login_script_path.is_empty(),
        first_login_script_path: spec.first_login_script_path.clone(),
        import_custom_drivers: !spec.custom_drivers_path.is_empty(),
        custom_drivers_path: spec.custom_drivers_path.clone(),
        import_storage_controller_drivers: spec.import_storage_controller_drivers,
        import_registry_file: !spec.registry_file_path.is_empty(),
        registry_file_path: spec.registry_file_path.clone(),
        import_custom_files: !spec.custom_files_path.is_empty(),
        custom_files_path: spec.custom_files_path.clone(),
        custom_username: !spec.username.is_empty(),
        username: spec.username.clone(),
        custom_volume_label: !spec.volume_label.is_empty(),
        volume_label: spec.volume_label.clone(),
        win7_fix_acpi_bsod: spec.win7_fix_acpi_bsod,
        win7_inject_usb3_driver: spec.win7_inject_usb3_driver,
        win7_usb3_driver_path: spec.win7_usb3_driver_path.clone(),
        win7_inject_nvme_driver: spec.win7_inject_nvme_driver,
        win7_nvme_driver_path: spec.win7_nvme_driver_path.clone(),
        win7_fix_storage_bsod: spec.win7_fix_storage_bsod,
        win7_uefi_patch: spec.win7_uefi_patch,
        xp_inject_usb3_driver: spec.xp_inject_usb3_driver,
        xp_inject_nvme_driver: spec.xp_inject_nvme_driver,
        ..AdvancedOptionsData::default()
    };
    value.builtin_administrator.enabled = spec.builtin_administrator.enabled;
    value.builtin_administrator.account_name = spec.builtin_administrator.account_name.clone();
    value.builtin_administrator.password = spec.builtin_administrator.password.clone().into();
    value.builtin_administrator.auto_logon = spec.builtin_administrator.auto_logon;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_never_reenables_diskpart_scripts() {
        let spec: InstallSpec =
            serde_json::from_str(r#"{"target_partition":"C:","image_path":"D:\\a.wim"}"#).unwrap();
        let (prefs, warnings) = prefs_from_spec(&spec).unwrap();
        assert!(!prefs.run_diskpart_scripts);
        assert!(!prefs.auto_reboot);
        assert!(prefs.format_partition);
        assert!(prefs.repair_boot);
        assert!(warnings.is_empty());
    }

    #[test]
    fn unattended_automation_restart_is_delayed_and_forcing_only_when_opted_in() {
        assert_eq!(restart_policy(false), (5, false));
        assert_eq!(restart_policy(true), (60, true));
    }

    fn dual_boot_source_partition() -> Partition {
        Partition {
            letter: "C:".to_owned(),
            total_size_mb: 80 * 1024,
            free_size_mb: 60 * 1024,
            free_size_bytes: 60 * lr_core::custom_install::GIB,
            label: "Windows".to_owned(),
            is_system_partition: true,
            has_windows: true,
            partition_style: crate::core::disk::PartitionStyle::GPT,
            disk_number: Some(0),
            partition_number: Some(3),
            disk_size_bytes: Some(80 * lr_core::custom_install::GIB),
            partition_offset_bytes: Some(331_350_016),
            partition_size_bytes: Some(79 * lr_core::custom_install::GIB),
            partition_kind: Some(lr_core::windows_storage::PartitionKind::BasicData),
            install_target_eligible: true,
            storage_media: lr_core::data_staging::StorageMedia::Unknown,
            stable_identity: None,
            bitlocker_status: VolumeStatus::NotEncrypted,
        }
    }

    #[test]
    fn cli_dual_boot_uses_the_explicit_integer_gib_and_shared_image_minimum() {
        let spec: InstallSpec = serde_json::from_str(
            r#"{"target_partition":"C:","install_mode":"dual_boot","dual_boot_size_gib":24,"image_path":"D:\\a.wim"}"#,
        )
        .unwrap();
        let image =
            lr_core::custom_install::image_space_requirement(20 * lr_core::custom_install::GIB, 0);
        let plan =
            custom_install_plan_from_spec(&spec, &dual_boot_source_partition(), image).unwrap();
        let lr_core::custom_install::CustomInstallPlan::DualBoot(plan) = plan else {
            panic!()
        };
        assert_eq!(plan.target_length_bytes, 24 * lr_core::custom_install::GIB);
        assert_eq!(
            dual_boot_size_gib_for_output(&lr_core::custom_install::CustomInstallPlan::DualBoot(
                plan
            )),
            Some(24)
        );

        let too_small: InstallSpec = serde_json::from_str(
            r#"{"target_partition":"C:","install_mode":"dual_boot","dual_boot_size_gib":21,"image_path":"D:\\a.wim"}"#,
        )
        .unwrap();
        assert!(
            custom_install_plan_from_spec(&too_small, &dual_boot_source_partition(), image)
                .is_err()
        );
    }

    #[test]
    fn stable_software_ids_resolve_only_unique_live_catalogue_entries() {
        let catalogue = crate::download::config::ConfigManager::load_from_content_with_soft(
            None,
            None,
            Some(
                r#"{"categories":[{"id":"apps","name":"Apps","items":[{"id":"todesk","name":"ToDesk","description":"remote","download_url":"https://example.test/todesk.exe","filename":"ToDesk.exe","silent_command":"\"{installer}\" /S","requires_admin":true},{"id":"7zip-x64","name":"7-Zip","description":"archive","download_url":"https://example.test/7z.exe","filename":"7z.exe","silent_command":"\"{installer}\" /S"},{"id":"bandizip-x64","name":"Bandizip","description":"archive","download_url":"https://example.test/bandizip.exe","filename":"bandizip.exe","silent_command":"\"{installer}\" /S"}]}]}"#,
            ),
        );
        let ids = vec![
            "todesk".to_owned(),
            "7zip-x64".to_owned(),
            "bandizip-x64".to_owned(),
        ];
        let selected = resolve_preinstalled_software_from_catalog(&ids, false, &catalogue).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|value| value.id.as_str())
                .collect::<Vec<_>>(),
            ["todesk", "7zip-x64", "bandizip-x64"]
        );
        assert!(resolve_preinstalled_software_from_catalog(
            &["missing".to_owned()],
            false,
            &catalogue
        )
        .is_err());
    }

    #[test]
    fn unavailable_optional_software_catalogue_is_a_bounded_continue_warning() {
        let warning = optional_software_catalogue_warning(&[
            "attempt 1/3: dns".to_owned(),
            "attempt 2/3: dns".to_owned(),
            "attempt 3/3: dns".to_owned(),
        ]);
        assert!(warning.contains("core Windows installation will continue"));
        assert!(warning.contains("attempt 3/3"));
    }
}
