//! User-facing command-line grammar and JSON protocol for the normal-Windows executable.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use super::cli_config::{
    BackupSpec, CliBackupExecutionMode, CliBackupFormat, CliBackupOutputPolicy, CliConfig,
    CliOperation, InstallSpec, CLI_CONFIG_SCHEMA_VERSION,
};

pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_CONFIG: i32 = 3;
pub const EXIT_PREFLIGHT: i32 = 4;
pub const EXIT_EXECUTION: i32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Plan,
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAction {
    Generate,
    Validate,
    Normalize,
    Show,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    Disks,
    Image,
    PeCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Install {
        action: Action,
        config: PathBuf,
        dry_run: bool,
        yes: bool,
    },
    Backup {
        action: Action,
        config: PathBuf,
        dry_run: bool,
        yes: bool,
    },
    Config {
        action: ConfigAction,
        options: BTreeMap<String, String>,
        flags: Vec<String>,
    },
    Inspect {
        kind: InspectKind,
        options: BTreeMap<String, String>,
    },
    Update {
        action: UpdateAction,
    },
    Tool(super::cli_tools::ToolInvocation),
    LegacyInstall,
}

pub fn parse(args: &[String]) -> Result<Option<Invocation>> {
    let Some(first) = args.get(1).map(String::as_str) else {
        return Ok(None);
    };
    if matches!(first, "--help" | "-h" | "help") {
        if args.len() != 2 {
            return Err(anyhow!("help does not accept additional arguments"));
        }
        return Ok(Some(Invocation::Help));
    }
    if first.eq_ignore_ascii_case("--install") || first.eq_ignore_ascii_case("/INSTALL") {
        return Ok(Some(Invocation::LegacyInstall));
    }
    match first {
        "install" => parse_operation(args, true).map(Some),
        "backup" => parse_operation(args, false).map(Some),
        "config" => parse_config(args).map(Some),
        "inspect" => parse_inspect(args).map(Some),
        "update" => parse_update(args).map(Some),
        "tool" => super::cli_tools::parse(args)
            .map(Invocation::Tool)
            .map(Some),
        _ => Ok(None),
    }
}

fn parse_update(args: &[String]) -> Result<Invocation> {
    if args.get(2).map(String::as_str) != Some("restore") || args.len() != 3 {
        return Err(anyhow!("expected exactly 'update restore'"));
    }
    Ok(Invocation::Update {
        action: UpdateAction::Restore,
    })
}

fn parse_inspect(args: &[String]) -> Result<Invocation> {
    let kind = match args.get(2).map(String::as_str) {
        Some("disks") => InspectKind::Disks,
        Some("image") => InspectKind::Image,
        Some("pe-cache") => InspectKind::PeCache,
        _ => return Err(anyhow!("expected inspect disks|image|pe-cache")),
    };
    let (options, flags) = collect_options(&args[3..])?;
    reject_flags(&flags, &[])?;
    if kind == InspectKind::Image {
        reject_unrecognized(&options, &["path"])?;
    } else {
        reject_unrecognized(&options, &[])?;
    }
    Ok(Invocation::Inspect { kind, options })
}

/// Parses and, when recognized, executes a user-facing normal-endpoint CLI request.
/// Parse failures are also emitted as JSON and use the stable usage exit code.
pub fn execute_args(args: &[String]) -> Option<i32> {
    match parse(args) {
        Ok(Some(
            invocation @ (Invocation::Help | Invocation::LegacyInstall | Invocation::Config { .. }),
        )) => {
            ensure_parent_console();
            Some(execute(invocation))
        }
        Ok(Some(_)) => None,
        Ok(None) => None,
        Err(error) => {
            ensure_parent_console();
            let written = print_json(
                json!({"ok":false,"error":{"code":"usage","message":error.to_string()}}),
            );
            Some(if written { EXIT_USAGE } else { EXIT_EXECUTION })
        }
    }
}

/// Executes read-only commands that depend on the configured WIM runtime and fresh inventory.
pub fn execute_runtime_args(args: &[String]) -> Option<i32> {
    match parse(args) {
        Ok(Some(invocation))
            if matches!(
                &invocation,
                Invocation::Inspect { .. }
                    | Invocation::Install {
                        action: Action::Plan,
                        ..
                    }
                    | Invocation::Backup {
                        action: Action::Plan,
                        ..
                    }
            ) || matches!(&invocation, Invocation::Tool(tool) if !tool.is_live()) =>
        {
            ensure_parent_console();
            Some(execute(invocation))
        }
        _ => None,
    }
}

pub fn is_run_request(args: &[String]) -> bool {
    match parse(args) {
        Ok(Some(Invocation::Tool(tool))) => tool.is_live(),
        Ok(Some(invocation)) => matches!(
            invocation,
            Invocation::Install {
                action: Action::Run,
                ..
            } | Invocation::Backup {
                action: Action::Run,
                ..
            } | Invocation::Update { .. }
        ),
        _ => false,
    }
}

/// Returns true only when a public run command may cross into the production executor.
/// `run --dry-run` is deliberately a read-only alias for `plan` and must not trigger UAC or the
/// development-build destructive-operation guard.
pub fn is_destructive_run_request(args: &[String]) -> bool {
    match parse(args) {
        Ok(Some(Invocation::Tool(tool))) => tool.is_live(),
        Ok(Some(invocation)) => matches!(
            invocation,
            Invocation::Install {
                action: Action::Run,
                dry_run: false,
                ..
            } | Invocation::Backup {
                action: Action::Run,
                dry_run: false,
                ..
            } | Invocation::Update { .. }
        ),
        _ => false,
    }
}

pub fn requires_administrator(args: &[String]) -> bool {
    match parse(args) {
        Ok(Some(Invocation::Tool(tool))) => tool.requires_administrator(),
        _ => is_destructive_run_request(args),
    }
}

pub fn execute_run_args(args: &[String]) -> Option<i32> {
    match parse(args) {
        Ok(Some(invocation))
            if matches!(
                &invocation,
                Invocation::Install {
                    action: Action::Run,
                    ..
                } | Invocation::Backup {
                    action: Action::Run,
                    ..
                } | Invocation::Update { .. }
            ) || matches!(&invocation, Invocation::Tool(tool) if tool.is_live()) =>
        {
            ensure_parent_console();
            Some(execute(invocation))
        }
        _ => None,
    }
}

pub fn administrator_required() -> i32 {
    administrator_required_for("install/backup/toolbox run, BitLocker key read, or update restore")
}

pub fn administrator_required_for(command: &str) -> i32 {
    ensure_parent_console();
    let written = print_json(
        json!({"ok":false,"error":{"code":"administrator_required","message":format!("{command} must be started from an already elevated administrator console; LetRecovery never auto-elevates CLI commands")}}),
    );
    if written {
        EXIT_USAGE
    } else {
        EXIT_EXECUTION
    }
}

pub fn startup_usage_error(message: impl std::fmt::Display) -> i32 {
    ensure_parent_console();
    let written =
        print_json(json!({"ok":false,"error":{"code":"usage","message":message.to_string()}}));
    if written {
        EXIT_USAGE
    } else {
        EXIT_EXECUTION
    }
}

pub fn development_run_denied() -> i32 {
    ensure_parent_console();
    let _ = print_json(
        json!({"ok":false,"error":{"code":"development_build_denied","message":"non-elevated test builds cannot execute install/backup run or update restore"}}),
    );
    EXIT_EXECUTION
}

fn parse_operation(args: &[String], install: bool) -> Result<Invocation> {
    let action = match args.get(2).map(String::as_str) {
        Some("plan") => Action::Plan,
        Some("run") => Action::Run,
        _ => return Err(anyhow!("expected '{} plan' or '{} run'", args[1], args[1])),
    };
    let (options, flags) = collect_options(&args[3..])?;
    let config = options
        .get("config")
        .ok_or_else(|| anyhow!("--config <path> is required"))?;
    reject_unrecognized(&options, &["config"])?;
    reject_flags(&flags, &["dry-run", "yes"])?;
    if action == Action::Plan && !flags.is_empty() {
        return Err(anyhow!("plan does not accept --yes or --dry-run"));
    }
    let invocation = if install {
        Invocation::Install {
            action,
            config: config.into(),
            dry_run: flags.contains(&"dry-run".to_owned()),
            yes: flags.contains(&"yes".to_owned()),
        }
    } else {
        Invocation::Backup {
            action,
            config: config.into(),
            dry_run: flags.contains(&"dry-run".to_owned()),
            yes: flags.contains(&"yes".to_owned()),
        }
    };
    Ok(invocation)
}

fn parse_config(args: &[String]) -> Result<Invocation> {
    let action = match args.get(2).map(String::as_str) {
        Some("generate") => ConfigAction::Generate,
        Some("validate") => ConfigAction::Validate,
        Some("normalize") => ConfigAction::Normalize,
        Some("show") => ConfigAction::Show,
        _ => return Err(anyhow!("expected config generate|validate|normalize|show")),
    };
    let (options, flags) = collect_options(&args[3..])?;
    Ok(Invocation::Config {
        action,
        options,
        flags,
    })
}

pub fn execute(invocation: Invocation) -> i32 {
    match execute_inner(invocation) {
        Ok((command, data)) => {
            if print_json(json!({"ok":true,"command":command,"data":data})) {
                EXIT_OK
            } else {
                EXIT_EXECUTION
            }
        }
        Err(failure) => {
            if print_json(
                json!({"ok":false,"error":{"code":failure.code,"message":failure.message}}),
            ) {
                failure.exit
            } else {
                EXIT_EXECUTION
            }
        }
    }
}

#[derive(Debug)]
struct Failure {
    exit: i32,
    code: &'static str,
    message: String,
}

fn execute_inner(invocation: Invocation) -> std::result::Result<(&'static str, Value), Failure> {
    match invocation {
        Invocation::Help => Ok(("help", json!({"text": help_text()}))),
        Invocation::LegacyInstall => Err(fail(EXIT_USAGE, "legacy_cli_removed", "legacy --install --config is no longer accepted because it bypassed native safety planning; migrate the file to schema_version 1 and run 'install plan --config <file>', then 'install run --config <file> --yes'")),
        Invocation::Config { action, options, flags } => execute_config(action, options, flags),
        Invocation::Inspect { kind, options } => execute_inspect(kind, options),
        Invocation::Tool(tool) => {
            let command = if tool.name == "list" {
                "tool.list"
            } else if tool.is_live() {
                "tool.run"
            } else {
                "tool.inspect"
            };
            let exit = if tool.is_live() { EXIT_EXECUTION } else { EXIT_PREFLIGHT };
            super::cli_tools::execute(&tool)
                .map(|value| (command, value))
                .map_err(|error| fail(exit, "tool_failed", error))
        }
        Invocation::Update {
            action: UpdateAction::Restore,
        } => {
            let report = super::cli_update::restore_current_windows_update()
                .map_err(|error| fail(EXIT_EXECUTION, "update_restore_failed", error))?;
            let partial = !report.warnings.is_empty() || !report.missing_services.is_empty();
            let data = json!({
                "outcome": if partial { "completed_with_warnings" } else { "completed" },
                "restored_values": report.applied_values,
                "already_restored_values": report.already_applied_values,
                "baseline_reused": report.baseline_reused,
                "missing_services": report.missing_services,
                "warnings": report.warnings,
            });
            if partial {
                emit_progress(json!({
                    "event":"warning",
                    "code":"update_restore_partial",
                    "data":data,
                }));
            }
            Ok(("update.restore", data))
        }
        Invocation::Install { action, config, dry_run, yes } => {
            let config_path = config;
            let config = load_for_operation(&config_path, "install")?;
            if action == Action::Run {
                if let CliOperation::Install(spec) = &config.operation {
                    if !spec.advanced.builtin_administrator.password.is_empty() {
                        super::cli_config::verify_sensitive_config_acl(&config_path)
                            .map_err(|error| fail(EXIT_CONFIG, "insecure_sensitive_config", error))?;
                    }
                }
            }
            let CliOperation::Install(spec) = config.operation else { unreachable!() };
            let automation_shutdown = action == Action::Run
                && !dry_run
                && yes
                && spec.automation_shutdown_on_terminal;
            let prepared = super::cli_install::plan_install(&spec).map_err(|error| {
                automation_failure(
                    fail(EXIT_PREFLIGHT, "install_preflight_failed", error),
                    automation_shutdown,
                )
            })?;
            let plan = super::cli_install::install_plan_json(&prepared);
            if action == Action::Plan || dry_run { return Ok(("install.plan", plan)); }
            if !yes { return Err(fail(EXIT_USAGE, "confirmation_required", "install run requires explicit --yes; use --dry-run to inspect the plan")); }
            let result = super::cli_install::run_install(prepared)
                .map_err(|error| automation_failure(
                    fail(EXIT_EXECUTION, "install_execution_failed", error),
                    automation_shutdown,
                ))?;
            Ok(("install.run", json!({"plan":plan,"result":result})))
        }
        Invocation::Backup { action, config, dry_run, yes } => {
            let config = load_for_operation(&config, "backup")?;
            let CliOperation::Backup(spec) = config.operation else { unreachable!() };
            let prepared = super::cli_backup::plan_backup(&spec)
                .map_err(|error| fail(EXIT_PREFLIGHT, "backup_preflight_failed", error))?;
            let plan = super::cli_backup::backup_plan_json(&prepared);
            if action == Action::Plan || dry_run { return Ok(("backup.plan", plan)); }
            if !yes { return Err(fail(EXIT_USAGE, "confirmation_required", "backup run requires explicit --yes; use --dry-run to inspect the plan")); }
            let result = super::cli_backup::run_backup(prepared)
                .map_err(|error| fail(EXIT_EXECUTION, "backup_execution_failed", error))?;
            Ok(("backup.run", json!({"plan":plan,"result":result})))
        }
    }
}

fn automation_failure(failure: Failure, enabled: bool) -> Failure {
    if enabled {
        match lr_core::windows_shutdown::schedule_shutdown(
            15,
            "LetRecovery automation reached a terminal failure; this test machine will power off.",
        ) {
            Ok(()) => emit_progress(json!({
                "event":"automation_shutdown_scheduled",
                "timeout_seconds":15,
                "terminal":"failure",
            })),
            Err(error) => emit_progress(json!({
                "event":"warning",
                "code":"automation_shutdown_not_scheduled",
                "message":error.to_string(),
            })),
        }
    }
    failure
}

fn execute_inspect(
    kind: InspectKind,
    options: BTreeMap<String, String>,
) -> std::result::Result<(&'static str, Value), Failure> {
    match kind {
        InspectKind::Disks => {
            let items = super::disk::DiskManager::get_partitions()
                .map_err(|error| fail(EXIT_PREFLIGHT, "disk_inventory_failed", error))?;
            let partitions = items
                .into_iter()
                .map(|partition| {
                    let stable_identity_digest = partition.stable_identity.map(|identity| {
                        let digest = blake3::hash(format!("{identity:?}").as_bytes()).to_hex();
                        digest[..16].to_owned()
                    });
                    json!({
                        "partition":partition.letter,"disk_number":partition.disk_number,"partition_number":partition.partition_number,
                        "disk_size_bytes":partition.disk_size_bytes,"offset_bytes":partition.partition_offset_bytes,"length_bytes":partition.partition_size_bytes,
                        "system":partition.is_system_partition,"has_windows":partition.has_windows,"style":format!("{:?}",partition.partition_style),
                        "bitlocker":format!("{:?}",partition.bitlocker_status),"stable_identity_digest":stable_identity_digest
                    })
                })
                .collect::<Vec<_>>();
            Ok(("inspect.disks", json!({"partitions":partitions})))
        }
        InspectKind::Image => {
            let path = required_path(&options, "path")?;
            super::cli_config::require_plain_regular_file(&path)
                .map_err(|error| fail(EXIT_PREFLIGHT, "image_path_rejected", error))?;
            let images = super::dism::Dism::new()
                .get_image_info(&path.to_string_lossy())
                .map_err(|error| fail(EXIT_PREFLIGHT, "image_inventory_failed", error))?
                .into_iter()
                .filter(super::dism::is_installable_image)
                .map(|image| json!({"index":image.index,"name":image.name,"size_bytes":image.size_bytes,"installation_type":image.installation_type,"major":image.major_version,"minor":image.minor_version,"build":image.build,"architecture":image.architecture}))
                .collect::<Vec<_>>();
            Ok(("inspect.image", json!({"path":path,"images":images})))
        }
        InspectKind::PeCache => {
            let entries = crate::download::config::PeCache::load_strict()
                .map_err(|error| fail(EXIT_CONFIG, "pe_cache_config_invalid", error))?
                .unwrap_or_default()
                .into_iter()
                .map(|pe| {
                    let status = match super::pe::PeManager::check_cached_pe(
                        &pe.filename,
                        pe.sha256.as_deref(),
                        pe.md5.as_deref(),
                    ) {
                        Ok(lr_core::cached_artifact::CachedArtifactStatus::Ready { .. }) => "ready",
                        Ok(lr_core::cached_artifact::CachedArtifactStatus::Missing) => "missing",
                        Err(_) => "rejected",
                    };
                    json!({"display_name":pe.display_name,"filename":pe.filename,"status":status,"sha256_declared":pe.sha256.is_some(),"md5_declared":pe.md5.is_some()})
                })
                .collect::<Vec<_>>();
            Ok(("inspect.pe-cache", json!({"entries":entries})))
        }
    }
}

fn execute_config(
    action: ConfigAction,
    options: BTreeMap<String, String>,
    flags: Vec<String>,
) -> std::result::Result<(&'static str, Value), Failure> {
    match action {
        ConfigAction::Validate => {
            reject_config_args(&options, &flags, &["config"], &[])?;
            let path = required_path(&options, "config")?;
            let config = CliConfig::load(&path)
                .map_err(|error| fail(EXIT_CONFIG, "invalid_config", error))?;
            Ok((
                "config.validate",
                json!({"path":path,"schema_version":config.schema_version,"operation":operation_name(&config.operation)}),
            ))
        }
        ConfigAction::Show => {
            reject_config_args(&options, &flags, &["config"], &[])?;
            let path = required_path(&options, "config")?;
            let config = CliConfig::load(&path)
                .map_err(|error| fail(EXIT_CONFIG, "invalid_config", error))?;
            Ok(("config.show", config.redacted_value()))
        }
        ConfigAction::Normalize => {
            reject_config_args(&options, &flags, &["config", "output"], &["force"])?;
            let input = required_path(&options, "config")?;
            let output = if options.contains_key("output") {
                required_path(&options, "output")?
            } else {
                input.clone()
            };
            let config = CliConfig::load(&input)
                .map_err(|error| fail(EXIT_CONFIG, "invalid_config", error))?;
            config
                .write_atomic(&output, flags.iter().any(|flag| flag == "force"))
                .map_err(|error| fail(EXIT_CONFIG, "write_failed", error))?;
            Ok((
                "config.normalize",
                json!({"path":output,"preview":config.redacted_value()}),
            ))
        }
        ConfigAction::Generate => generate_config(options, flags),
    }
}

fn generate_config(
    mut options: BTreeMap<String, String>,
    flags: Vec<String>,
) -> std::result::Result<(&'static str, Value), Failure> {
    reject_flags(&flags, &["force", "interactive"])
        .map_err(|error| fail(EXIT_USAGE, "usage", error))?;
    if flags.iter().any(|flag| flag == "interactive") {
        fill_interactively(&mut options)
            .map_err(|error| fail(EXIT_USAGE, "interactive_input_failed", error))?;
    }
    let output = required_path(&options, "output")?;
    let operation = options
        .get("operation")
        .map(String::as_str)
        .ok_or_else(|| {
            fail(
                EXIT_USAGE,
                "missing_option",
                "--operation install|backup is required",
            )
        })?;
    let config = match operation {
        "install" => {
            reject_config_args(
                &options,
                &flags,
                &[
                    "operation",
                    "output",
                    "target-partition",
                    "install-mode",
                    "confirmed-disk-numbers",
                    "dual-boot-size-gib",
                    "image-path",
                    "image-backing-path",
                    "volume-index",
                    "format-partition",
                    "repair-boot",
                    "unattended",
                    "auto-reboot",
                    "automation-shutdown-on-terminal",
                    "driver-action",
                    "boot-mode",
                    "boot-pca-mode",
                    "custom-unattend-path",
                    "inherit-app-install-prefs",
                    "preinstalled-software-ids",
                    "remove-shortcut-arrow",
                    "restore-classic-context-menu",
                    "bypass-nro",
                    "disable-windows-update",
                    "disable-windows-defender",
                    "disable-reserved-storage",
                    "disable-uac",
                    "disable-device-encryption",
                    "remove-uwp-apps",
                    "install-vmware-tools",
                    "deploy-script-path",
                    "first-login-script-path",
                    "custom-drivers-path",
                    "import-storage-controller-drivers",
                    "registry-file-path",
                    "custom-files-path",
                    "username",
                    "builtin-administrator-enabled",
                    "builtin-administrator-account-name",
                    "builtin-administrator-auto-logon",
                    "volume-label",
                    "win7-fix-acpi-bsod",
                    "win7-inject-usb3-driver",
                    "win7-usb3-driver-path",
                    "win7-inject-nvme-driver",
                    "win7-nvme-driver-path",
                    "win7-fix-storage-bsod",
                    "win7-uefi-patch",
                    "xp-inject-usb3-driver",
                    "xp-inject-nvme-driver",
                ],
                &["force", "interactive"],
            )?;
            let driver_action = match options
                .get("driver-action")
                .map(String::as_str)
                .unwrap_or("auto_import")
            {
                "none" => super::cli_config::CliDriverAction::None,
                "save_only" => super::cli_config::CliDriverAction::SaveOnly,
                "auto_import" => super::cli_config::CliDriverAction::AutoImport,
                _ => {
                    return Err(fail(
                        EXIT_USAGE,
                        "invalid_option",
                        "--driver-action must be none|save_only|auto_import",
                    ))
                }
            };
            let boot_mode = match options
                .get("boot-mode")
                .map(String::as_str)
                .unwrap_or("auto")
            {
                "auto" => super::cli_config::CliBootMode::Auto,
                "uefi" => super::cli_config::CliBootMode::Uefi,
                "legacy" => super::cli_config::CliBootMode::Legacy,
                _ => {
                    return Err(fail(
                        EXIT_USAGE,
                        "invalid_option",
                        "--boot-mode must be auto|uefi|legacy",
                    ))
                }
            };
            let boot_pca_mode = match options
                .get("boot-pca-mode")
                .map(String::as_str)
                .unwrap_or("auto")
            {
                "auto" => super::cli_config::CliBootPcaMode::Auto,
                "pca2011" => super::cli_config::CliBootPcaMode::Pca2011,
                "pca2023" => super::cli_config::CliBootPcaMode::Pca2023,
                _ => {
                    return Err(fail(
                        EXIT_USAGE,
                        "invalid_option",
                        "--boot-pca-mode must be auto|pca2011|pca2023",
                    ))
                }
            };
            let install_mode = match options
                .get("install-mode")
                .map(String::as_str)
                .unwrap_or("reinstall_partition")
            {
                "reinstall_partition" => super::cli_config::CliInstallMode::ReinstallPartition,
                "repartition_all_disks" => super::cli_config::CliInstallMode::RepartitionAllDisks,
                "dual_boot" => super::cli_config::CliInstallMode::DualBoot,
                _ => return Err(fail(
                    EXIT_USAGE,
                    "invalid_option",
                    "--install-mode must be reinstall_partition|repartition_all_disks|dual_boot",
                )),
            };
            let confirmed_disk_numbers = options
                .get("confirmed-disk-numbers")
                .map(|value| {
                    value
                        .split(',')
                        .map(|part| {
                            part.trim().parse::<u32>().map_err(|_| {
                                fail(
                                    EXIT_USAGE,
                                    "invalid_option",
                                    "--confirmed-disk-numbers must be a comma-separated list of integers",
                                )
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            let dual_boot_size_gib = options
                .get("dual-boot-size-gib")
                .map(|value| {
                    value.parse::<u64>().map_err(|_| {
                        fail(
                            EXIT_USAGE,
                            "invalid_option",
                            "--dual-boot-size-gib must be a positive integer",
                        )
                    })
                })
                .transpose()?;
            let mut advanced = super::cli_config::AdvancedSpec {
                remove_shortcut_arrow: bool_option(&options, "remove-shortcut-arrow", false)?,
                restore_classic_context_menu: bool_option(
                    &options,
                    "restore-classic-context-menu",
                    false,
                )?,
                bypass_nro: bool_option(&options, "bypass-nro", false)?,
                disable_windows_update: bool_option(&options, "disable-windows-update", false)?,
                disable_windows_defender: bool_option(&options, "disable-windows-defender", false)?,
                disable_reserved_storage: bool_option(&options, "disable-reserved-storage", false)?,
                disable_uac: bool_option(&options, "disable-uac", false)?,
                disable_device_encryption: bool_option(
                    &options,
                    "disable-device-encryption",
                    false,
                )?,
                remove_uwp_apps: bool_option(&options, "remove-uwp-apps", false)?,
                install_vmware_tools: bool_option(&options, "install-vmware-tools", false)?,
                deploy_script_path: option(&options, "deploy-script-path"),
                first_login_script_path: option(&options, "first-login-script-path"),
                custom_drivers_path: option(&options, "custom-drivers-path"),
                import_storage_controller_drivers: bool_option(
                    &options,
                    "import-storage-controller-drivers",
                    false,
                )?,
                registry_file_path: option(&options, "registry-file-path"),
                custom_files_path: option(&options, "custom-files-path"),
                username: option(&options, "username"),
                volume_label: option(&options, "volume-label"),
                win7_fix_acpi_bsod: bool_option(&options, "win7-fix-acpi-bsod", false)?,
                win7_inject_usb3_driver: bool_option(&options, "win7-inject-usb3-driver", false)?,
                win7_usb3_driver_path: option(&options, "win7-usb3-driver-path"),
                win7_inject_nvme_driver: bool_option(&options, "win7-inject-nvme-driver", false)?,
                win7_nvme_driver_path: option(&options, "win7-nvme-driver-path"),
                win7_fix_storage_bsod: bool_option(&options, "win7-fix-storage-bsod", false)?,
                win7_uefi_patch: bool_option(&options, "win7-uefi-patch", false)?,
                xp_inject_usb3_driver: bool_option(&options, "xp-inject-usb3-driver", false)?,
                xp_inject_nvme_driver: bool_option(&options, "xp-inject-nvme-driver", false)?,
                ..Default::default()
            };
            advanced.builtin_administrator.enabled =
                bool_option(&options, "builtin-administrator-enabled", false)?;
            advanced.builtin_administrator.account_name = options
                .get("builtin-administrator-account-name")
                .cloned()
                .unwrap_or_else(|| "Administrator".to_owned());
            advanced.builtin_administrator.auto_logon =
                bool_option(&options, "builtin-administrator-auto-logon", true)?;
            CliConfig {
                schema_version: CLI_CONFIG_SCHEMA_VERSION,
                operation: CliOperation::Install(Box::new(InstallSpec {
                    target_partition: required(&options, "target-partition")?,
                    install_mode,
                    confirmed_disk_numbers,
                    dual_boot_size_gib,
                    image_path: required(&options, "image-path")?,
                    image_backing_path: option(&options, "image-backing-path"),
                    volume_index: options
                        .get("volume-index")
                        .map(|v| v.parse())
                        .transpose()
                        .map_err(|_| {
                            fail(
                                EXIT_USAGE,
                                "invalid_option",
                                "--volume-index must be an integer",
                            )
                        })?
                        .unwrap_or(1),
                    format_partition: bool_option(&options, "format-partition", true)?,
                    repair_boot: bool_option(&options, "repair-boot", true)?,
                    unattended: bool_option(&options, "unattended", false)?,
                    auto_reboot: bool_option(&options, "auto-reboot", false)?,
                    automation_shutdown_on_terminal: bool_option(
                        &options,
                        "automation-shutdown-on-terminal",
                        false,
                    )?,
                    driver_action,
                    boot_mode,
                    boot_pca_mode,
                    custom_unattend_path: option(&options, "custom-unattend-path"),
                    inherit_app_install_prefs: bool_option(
                        &options,
                        "inherit-app-install-prefs",
                        false,
                    )?,
                    preinstalled_software_ids: options
                        .get("preinstalled-software-ids")
                        .map(|value| value.split(',').map(str::to_owned).collect())
                        .unwrap_or_default(),
                    advanced,
                })),
            }
        }
        "backup" => {
            reject_config_args(
                &options,
                &flags,
                &[
                    "operation",
                    "output",
                    "source-partition",
                    "save-path",
                    "name",
                    "description",
                    "format",
                    "execution-mode",
                    "output-policy",
                    "auto-reboot",
                ],
                &["force", "interactive"],
            )?;
            let format = match options.get("format").map(String::as_str).unwrap_or("wim") {
                "wim" => CliBackupFormat::Wim,
                "esd" => CliBackupFormat::Esd,
                _ => {
                    return Err(fail(
                        EXIT_USAGE,
                        "invalid_option",
                        "--format must be wim|esd",
                    ))
                }
            };
            let execution_mode = backup_execution_mode_option(&options)?;
            let output_policy = backup_output_policy_option(&options)?;
            CliConfig {
                schema_version: CLI_CONFIG_SCHEMA_VERSION,
                operation: CliOperation::Backup(BackupSpec {
                    source_partition: required(&options, "source-partition")?,
                    save_path: required(&options, "save-path")?,
                    name: required(&options, "name")?,
                    description: option(&options, "description"),
                    format,
                    execution_mode,
                    output_policy,
                    auto_reboot: bool_option(&options, "auto-reboot", false)?,
                }),
            }
        }
        _ => {
            return Err(fail(
                EXIT_USAGE,
                "invalid_option",
                "--operation must be install or backup",
            ))
        }
    };
    let text = serde_json::to_string(&config)
        .map_err(|error| fail(EXIT_CONFIG, "serialization_failed", error))?;
    let config =
        CliConfig::parse(&text).map_err(|error| fail(EXIT_CONFIG, "invalid_config", error))?;
    config
        .write_atomic(&output, flags.iter().any(|flag| flag == "force"))
        .map_err(|error| fail(EXIT_CONFIG, "write_failed", error))?;
    Ok((
        "config.generate",
        json!({"path":output,"preview":config.redacted_value()}),
    ))
}

fn fill_interactively(options: &mut BTreeMap<String, String>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut stdout = std::io::stderr();
    for (key, prompt) in [
        ("operation", "Operation (install/backup)"),
        ("output", "Output config path"),
    ] {
        if !options.contains_key(key) {
            prompt_value(&mut input, &mut stdout, options, key, prompt)?;
        }
    }
    match options.get("operation").map(String::as_str) {
        Some("install") => {
            for (key, prompt) in [
                ("target-partition", "Target partition (for example C:)"),
                ("image-path", "Image path"),
            ] {
                if !options.contains_key(key) {
                    prompt_value(&mut input, &mut stdout, options, key, prompt)?;
                }
            }
        }
        Some("backup") => {
            for (key, prompt) in [
                ("source-partition", "Source partition"),
                ("save-path", "Destination image path"),
                ("name", "Backup name"),
            ] {
                if !options.contains_key(key) {
                    prompt_value(&mut input, &mut stdout, options, key, prompt)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn prompt_value(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    options: &mut BTreeMap<String, String>,
    key: &str,
    prompt: &str,
) -> Result<()> {
    let event = serde_json::to_string(&json!({
        "type": "prompt",
        "data": {"field": key, "message": prompt},
    }))?;
    writeln!(output, "{event}")?;
    output.flush()?;
    let mut value = String::new();
    if input.read_line(&mut value)? == 0 {
        return Err(anyhow!("interactive input ended while reading --{key}"));
    }
    options.insert(key.to_owned(), value.trim().to_owned());
    Ok(())
}

fn load_for_operation(path: &Path, expected: &str) -> std::result::Result<CliConfig, Failure> {
    let config = CliConfig::load(path).map_err(|e| fail(EXIT_CONFIG, "invalid_config", e))?;
    if operation_name(&config.operation) != expected {
        return Err(fail(
            EXIT_CONFIG,
            "operation_mismatch",
            format!(
                "configuration contains {}, expected {expected}",
                operation_name(&config.operation)
            ),
        ));
    }
    Ok(config)
}
fn operation_name(operation: &CliOperation) -> &'static str {
    match operation {
        CliOperation::Install(_) => "install",
        CliOperation::Backup(_) => "backup",
    }
}
fn option(options: &BTreeMap<String, String>, key: &str) -> String {
    options.get(key).cloned().unwrap_or_default()
}
fn bool_option(
    options: &BTreeMap<String, String>,
    key: &str,
    default: bool,
) -> std::result::Result<bool, Failure> {
    match options.get(key).map(String::as_str) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(fail(
            EXIT_USAGE,
            "invalid_option",
            format!("--{key} must be true or false"),
        )),
    }
}

fn backup_execution_mode_option(
    options: &BTreeMap<String, String>,
) -> std::result::Result<CliBackupExecutionMode, Failure> {
    match options
        .get("execution-mode")
        .map(String::as_str)
        .unwrap_or("auto")
    {
        "auto" => Ok(CliBackupExecutionMode::Auto),
        "direct" => Ok(CliBackupExecutionMode::Direct),
        "via_pe" => Ok(CliBackupExecutionMode::ViaPe),
        _ => Err(fail(
            EXIT_USAGE,
            "invalid_option",
            "--execution-mode must be auto|direct|via_pe",
        )),
    }
}

fn backup_output_policy_option(
    options: &BTreeMap<String, String>,
) -> std::result::Result<CliBackupOutputPolicy, Failure> {
    match options
        .get("output-policy")
        .map(String::as_str)
        .unwrap_or("create")
    {
        "create" => Ok(CliBackupOutputPolicy::Create),
        "replace" => Ok(CliBackupOutputPolicy::Replace),
        "append" => Ok(CliBackupOutputPolicy::Append),
        _ => Err(fail(
            EXIT_USAGE,
            "invalid_option",
            "--output-policy must be create|replace|append",
        )),
    }
}
fn required(options: &BTreeMap<String, String>, key: &str) -> std::result::Result<String, Failure> {
    options
        .get(key)
        .filter(|v| !v.trim().is_empty())
        .cloned()
        .ok_or_else(|| fail(EXIT_USAGE, "missing_option", format!("--{key} is required")))
}
fn required_path(
    options: &BTreeMap<String, String>,
    key: &str,
) -> std::result::Result<PathBuf, Failure> {
    let value = required(options, key)?;
    super::cli_config::validate_local_absolute_path_str(&value, key)
        .map_err(|error| fail(EXIT_USAGE, "invalid_path", error))?;
    Ok(PathBuf::from(value))
}
fn fail(exit: i32, code: &'static str, message: impl std::fmt::Display) -> Failure {
    Failure {
        exit,
        code,
        message: message.to_string(),
    }
}
fn print_json(value: Value) -> bool {
    let mut text = serde_json::to_string(&value).unwrap_or_else(|_| "{\"ok\":false}".to_owned());
    text.push('\n');
    write_stdout(text.as_bytes())
}

#[cfg(windows)]
fn ensure_parent_console() {
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // Failure normally means the process already owns a console or only inherited pipe handles.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn ensure_parent_console() {}

#[cfg(windows)]
fn write_stdout(bytes: &[u8]) -> bool {
    write_windows_stream(bytes, (-11i32) as u32)
}

#[cfg(windows)]
fn write_stderr(bytes: &[u8]) -> bool {
    write_windows_stream(bytes, (-12i32) as u32)
}

#[cfg(windows)]
fn write_windows_stream(bytes: &[u8], stream: u32) -> bool {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn WriteConsoleW(
            handle: *mut c_void,
            buffer: *const c_void,
            count: u32,
            written: *mut u32,
            reserved: *mut c_void,
        ) -> i32;
        fn WriteFile(
            handle: *mut c_void,
            buffer: *const c_void,
            count: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
    let handle = unsafe { GetStdHandle(stream) };
    if handle.is_null() || handle as isize == -1 {
        return false;
    }
    let mut mode = 0u32;
    if unsafe { GetConsoleMode(handle, &mut mode) } != 0 {
        let text = String::from_utf8_lossy(bytes);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut offset = 0usize;
        while offset < wide.len() {
            let count = (wide.len() - offset).min(u32::MAX as usize) as u32;
            let mut written = 0u32;
            if unsafe {
                WriteConsoleW(
                    handle,
                    wide[offset..].as_ptr().cast(),
                    count,
                    &mut written,
                    std::ptr::null_mut(),
                )
            } == 0
                || written == 0
            {
                return false;
            }
            offset += written as usize;
        }
    } else {
        let mut offset = 0usize;
        while offset < bytes.len() {
            let count = (bytes.len() - offset).min(u32::MAX as usize) as u32;
            let mut written = 0u32;
            if unsafe {
                WriteFile(
                    handle,
                    bytes[offset..].as_ptr().cast(),
                    count,
                    &mut written,
                    std::ptr::null_mut(),
                )
            } == 0
                || written == 0
            {
                return false;
            }
            offset += written as usize;
        }
    }
    true
}

#[cfg(not(windows))]
fn write_stdout(bytes: &[u8]) -> bool {
    std::io::stdout().write_all(bytes).is_ok()
}

#[cfg(not(windows))]
fn write_stderr(bytes: &[u8]) -> bool {
    std::io::stderr().write_all(bytes).is_ok()
}

pub fn emit_progress(value: Value) {
    let mut text = serde_json::to_string(&json!({"type":"progress","data":value}))
        .unwrap_or_else(|_| "{\"type\":\"progress_error\"}".to_owned());
    text.push('\n');
    let _ = write_stderr(text.as_bytes());
}

fn collect_options(args: &[String]) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut options = BTreeMap::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with("--") {
            return Err(anyhow!("unexpected positional argument {arg}"));
        }
        let key = arg.trim_start_matches("--").to_owned();
        if matches!(key.as_str(), "yes" | "dry-run" | "force" | "interactive") {
            if flags.iter().any(|existing| existing == &key) {
                return Err(anyhow!("duplicate flag {arg}"));
            }
            flags.push(key);
            i += 1;
        } else {
            let value = args
                .get(i + 1)
                .ok_or_else(|| anyhow!("{arg} requires a value"))?;
            if value.starts_with("--") {
                return Err(anyhow!("{arg} requires a value"));
            }
            if options.insert(key, value.clone()).is_some() {
                return Err(anyhow!("duplicate option {arg}"));
            }
            i += 2;
        }
    }
    Ok((options, flags))
}
fn reject_unrecognized(options: &BTreeMap<String, String>, allowed: &[&str]) -> Result<()> {
    if let Some(key) = options.keys().find(|k| !allowed.contains(&k.as_str())) {
        Err(anyhow!("unrecognized option --{key}"))
    } else {
        Ok(())
    }
}
fn reject_flags(flags: &[String], allowed: &[&str]) -> Result<()> {
    if let Some(flag) = flags.iter().find(|f| !allowed.contains(&f.as_str())) {
        Err(anyhow!("unrecognized flag --{flag}"))
    } else {
        Ok(())
    }
}
fn reject_config_args(
    options: &BTreeMap<String, String>,
    flags: &[String],
    allowed_options: &[&str],
    allowed_flags: &[&str],
) -> std::result::Result<(), Failure> {
    reject_unrecognized(options, allowed_options)
        .and_then(|_| reject_flags(flags, allowed_flags))
        .map_err(|e| fail(EXIT_USAGE, "usage", e))
}

pub fn help_text() -> &'static str {
    "LetRecovery command line (normal Windows only)\n\n  inspect disks\n  inspect image --path <image>\n  inspect pe-cache\n  install plan --config <file>\n  install run --config <file> [--yes] [--dry-run]\n  backup plan --config <file>\n  backup run --config <file> [--yes] [--dry-run]\n  update restore\n  tool list\n  config generate --operation install|backup --output <file> [operation flags] [--interactive] [--force]\n  config validate --config <file>\n  config normalize --config <file> [--output <file>] [--force]\n  config show --config <file>\n\nRun 'tool list' to enumerate all 22 toolbox CLI names; the full toolbox grammar is documented in the user guide. A real run, toolbox mutation, or update restore requires an already elevated administrator console; LetRecovery never auto-elevates public CLI commands. Install/backup/toolbox run also requires --yes, while install/backup run --dry-run is read-only. Install generation supports image/image-backing paths; explicit --install-mode reinstall_partition|repartition_all_disks|dual_boot; current-session --confirmed-disk-numbers only for full-disk mode; --dual-boot-size-gib only for dual-boot mode; format/repair/unattended/auto-reboot; driver action; boot/PCA; built-in Administrator non-secret fields; advanced options including VMware Tools and guarded Windows 7 USB3/NVMe/storage/UEFI controls; --inherit-app-install-prefs true|false; comma-separated stable v4 IDs through --preinstalled-software-ids; and disposable-VM terminal power-off through --automation-shutdown-on-terminal true|false. Passwords, Wi-Fi credentials, and BitLocker unlock secrets are never accepted on the command line; BitLocker CLI unlock reads its secret only from stdin, while GUI export may place supported secrets only in protected, redacted JSON. Driver action is the only driver-export/import selector unless GUI preferences are explicitly inherited. Backup schema/generation supports --execution-mode auto|direct|via_pe, --output-policy create|replace|append, and --auto-reboot true|false. Feature-gated unsafe combinations fail closed. Backup incremental booleans and SWM/GHO output formats are rejected. Boolean option values accept only true|false. Final results are one JSON object on stdout; sanitized progress is JSON Lines on stderr. Interactive prompting occurs only with --interactive."
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    fn parses_safe_run_controls() {
        let value = parse(&args(&[
            "lr",
            "install",
            "run",
            "--config",
            "a.json",
            "--yes",
            "--dry-run",
        ]))
        .unwrap()
        .unwrap();
        assert!(matches!(
            value,
            Invocation::Install {
                action: Action::Run,
                yes: true,
                dry_run: true,
                ..
            }
        ));
    }

    #[test]
    fn dry_run_is_never_classified_as_destructive() {
        let dry = args(&[
            "lr",
            "install",
            "run",
            "--config",
            "C:\\safe\\install.json",
            "--dry-run",
        ]);
        assert!(is_run_request(&dry));
        assert!(!is_destructive_run_request(&dry));

        let live = args(&[
            "lr",
            "install",
            "run",
            "--config",
            "C:\\safe\\install.json",
            "--yes",
        ]);
        assert!(is_destructive_run_request(&live));
    }

    #[test]
    fn update_restore_is_exact_and_requires_destructive_gate() {
        let command = args(&["lr", "update", "restore"]);
        assert!(matches!(
            parse(&command).unwrap(),
            Some(Invocation::Update {
                action: UpdateAction::Restore
            })
        ));
        assert!(is_run_request(&command));
        assert!(is_destructive_run_request(&command));
        assert!(parse(&args(&["lr", "update", "restore", "--yes"])).is_err());
        assert!(parse(&args(&["lr", "update", "status"])).is_err());
    }

    #[test]
    fn toolbox_routes_preserve_read_only_live_and_sensitive_admin_boundaries() {
        let read_only = args(&["lr", "tool", "network-info", "inspect"]);
        assert!(matches!(
            parse(&read_only).unwrap(),
            Some(Invocation::Tool(_))
        ));
        assert!(!is_run_request(&read_only));
        assert!(!is_destructive_run_request(&read_only));
        assert!(!requires_administrator(&read_only));

        let live = args(&["lr", "tool", "time-sync", "run", "--yes"]);
        assert!(is_run_request(&live));
        assert!(is_destructive_run_request(&live));
        assert!(requires_administrator(&live));

        let sensitive = args(&["lr", "tool", "bitlocker", "read-key", "--volume", "D:"]);
        assert!(!is_run_request(&sensitive));
        assert!(!is_destructive_run_request(&sensitive));
        assert!(requires_administrator(&sensitive));
    }
    #[test]
    fn legacy_install_is_deterministic_migration_error() {
        assert_eq!(execute(Invocation::LegacyInstall), EXIT_USAGE);
    }
    #[test]
    fn wizard_is_never_implicit() {
        let value = parse(&args(&[
            "lr",
            "config",
            "generate",
            "--operation",
            "install",
            "--output",
            "a.json",
            "--target-partition",
            "C:",
            "--image-path",
            "D:\\i.wim",
        ]))
        .unwrap()
        .unwrap();
        let Invocation::Config { flags, .. } = value else {
            panic!()
        };
        assert!(!flags.contains(&"interactive".to_owned()));
    }

    #[test]
    fn duplicate_flags_are_rejected() {
        assert!(parse(&args(&[
            "lr", "backup", "run", "--config", "a.json", "--yes", "--yes"
        ]))
        .is_err());
        assert!(parse(&args(&["lr", "config", "generate", "--force", "--force"])).is_err());
    }

    #[test]
    fn help_is_an_exact_command() {
        assert!(matches!(
            parse(&args(&["lr", "help"])).unwrap(),
            Some(Invocation::Help)
        ));
        assert!(parse(&args(&["lr", "help", "extra"])).is_err());
        assert!(parse(&args(&["lr", "--help", "--yes"])).is_err());
    }

    #[test]
    fn interactive_prompts_are_json_lines_and_eof_is_an_error() {
        let mut options = BTreeMap::new();
        let mut input = std::io::Cursor::new(b"install\n".to_vec());
        let mut output = Vec::new();
        prompt_value(
            &mut input,
            &mut output,
            &mut options,
            "operation",
            "Operation",
        )
        .unwrap();
        let event: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(event["type"], "prompt");
        assert_eq!(event["data"]["field"], "operation");
        assert_eq!(
            options.get("operation").map(String::as_str),
            Some("install")
        );

        let mut eof = std::io::Cursor::new(Vec::<u8>::new());
        let mut sink = Vec::new();
        assert!(prompt_value(&mut eof, &mut sink, &mut options, "output", "Output",).is_err());
    }

    #[test]
    fn backup_generator_mode_and_policy_are_strict_and_have_safe_defaults() {
        let empty = BTreeMap::new();
        assert_eq!(
            backup_execution_mode_option(&empty).unwrap(),
            CliBackupExecutionMode::Auto
        );
        assert_eq!(
            backup_output_policy_option(&empty).unwrap(),
            CliBackupOutputPolicy::Create
        );

        let mut values = BTreeMap::new();
        values.insert("execution-mode".to_owned(), "via_pe".to_owned());
        values.insert("output-policy".to_owned(), "append".to_owned());
        assert_eq!(
            backup_execution_mode_option(&values).unwrap(),
            CliBackupExecutionMode::ViaPe
        );
        assert_eq!(
            backup_output_policy_option(&values).unwrap(),
            CliBackupOutputPolicy::Append
        );

        values.insert("execution-mode".to_owned(), "ViaPE".to_owned());
        values.insert("output-policy".to_owned(), "overwrite".to_owned());
        assert!(backup_execution_mode_option(&values).is_err());
        assert!(backup_output_policy_option(&values).is_err());
    }

    #[test]
    fn generator_builds_a_rich_strict_config_but_never_accepts_password_arguments() {
        let root =
            std::env::temp_dir().join(format!("lr-cli-generator-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("install.json");
        let mut options = BTreeMap::from([
            ("operation".to_owned(), "install".to_owned()),
            ("output".to_owned(), output.display().to_string()),
            ("target-partition".to_owned(), "c:".to_owned()),
            ("image-path".to_owned(), r"D:\Images\install.wim".to_owned()),
            ("volume-index".to_owned(), "3".to_owned()),
            ("format-partition".to_owned(), "false".to_owned()),
            ("repair-boot".to_owned(), "true".to_owned()),
            ("driver-action".to_owned(), "save_only".to_owned()),
            ("boot-mode".to_owned(), "uefi".to_owned()),
            ("disable-windows-update".to_owned(), "true".to_owned()),
            ("disable-windows-defender".to_owned(), "true".to_owned()),
            ("remove-uwp-apps".to_owned(), "true".to_owned()),
            ("install-vmware-tools".to_owned(), "true".to_owned()),
            ("win7-inject-usb3-driver".to_owned(), "true".to_owned()),
            (
                "win7-usb3-driver-path".to_owned(),
                r"D:\Drivers\Win7Usb3".to_owned(),
            ),
            ("win7-inject-nvme-driver".to_owned(), "true".to_owned()),
            (
                "win7-nvme-driver-path".to_owned(),
                r"D:\Drivers\Win7Nvme".to_owned(),
            ),
            ("win7-fix-storage-bsod".to_owned(), "true".to_owned()),
            ("win7-uefi-patch".to_owned(), "true".to_owned()),
            (
                "builtin-administrator-enabled".to_owned(),
                "true".to_owned(),
            ),
        ]);
        let (command, preview) =
            generate_config(options.clone(), vec!["force".to_owned()]).unwrap();
        assert_eq!(command, "config.generate");
        assert_eq!(preview["preview"]["operation"]["volume_index"], 3);
        let generated = CliConfig::load(&output).unwrap();
        let CliOperation::Install(spec) = generated.operation else {
            panic!()
        };
        assert_eq!(spec.target_partition, "C:");
        assert_eq!(
            spec.driver_action,
            crate::core::cli_config::CliDriverAction::SaveOnly
        );
        assert!(spec.advanced.disable_windows_update);
        assert!(spec.advanced.disable_windows_defender);
        assert!(spec.advanced.remove_uwp_apps);
        assert!(spec.advanced.install_vmware_tools);
        assert!(spec.advanced.win7_inject_usb3_driver);
        assert_eq!(spec.advanced.win7_usb3_driver_path, r"D:\Drivers\Win7Usb3");
        assert!(spec.advanced.win7_inject_nvme_driver);
        assert_eq!(spec.advanced.win7_nvme_driver_path, r"D:\Drivers\Win7Nvme");
        assert!(spec.advanced.win7_fix_storage_bsod);
        assert!(spec.advanced.win7_uefi_patch);
        assert!(spec.advanced.builtin_administrator.enabled);
        assert!(spec.advanced.builtin_administrator.password.is_empty());

        options.insert(
            "builtin-administrator-password".to_owned(),
            "must-not-be-accepted".to_owned(),
        );
        assert!(generate_config(options, vec!["force".to_owned()]).is_err());
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn generator_emits_the_strict_dual_boot_capacity_contract() {
        let root =
            std::env::temp_dir().join(format!("lr-cli-dual-generator-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("dual.json");
        let options = BTreeMap::from([
            ("operation".to_owned(), "install".to_owned()),
            ("output".to_owned(), output.display().to_string()),
            ("target-partition".to_owned(), "C:".to_owned()),
            ("install-mode".to_owned(), "dual_boot".to_owned()),
            ("dual-boot-size-gib".to_owned(), "24".to_owned()),
            ("image-path".to_owned(), r"D:\Images\install.wim".to_owned()),
        ]);
        generate_config(options, vec!["force".to_owned()]).unwrap();
        let generated = CliConfig::load(&output).unwrap();
        let CliOperation::Install(spec) = generated.operation else {
            panic!()
        };
        assert_eq!(
            spec.install_mode,
            crate::core::cli_config::CliInstallMode::DualBoot
        );
        assert_eq!(spec.dual_boot_size_gib, Some(24));
        assert!(spec.confirmed_disk_numbers.is_empty());
        assert!(spec.repair_boot);

        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
