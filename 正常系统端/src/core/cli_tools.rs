//! Public JSON CLI adapter for every native toolbox entry.
//!
//! The CLI never reimplements a toolbox mutation. Read-only commands call the same inventory or
//! inspection boundary as the native dialog; mutating commands first build the same typed plan and
//! require an exact `run --yes` before entering the existing backend. Secrets are read from stdin,
//! never accepted as command-line values or included in progress/error output.

use std::collections::BTreeMap;
use std::io::{BufRead, Read};
use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::native_tool_executor::{
    plan_execution, ReadOnlyToolRequest, ReadOnlyToolResult, ToolExecutionPlan,
    ToolExecutionRequest,
};
use super::native_tools_controller::{
    plan_tool, NativeToolAction, ToolEnvironment, ToolSafetyClass,
};

pub const TOOL_NAMES: [(&str, NativeToolAction); 22] = [
    ("nvidia-driver", NativeToolAction::NvidiaDriverRemoval),
    ("partition-copy", NativeToolAction::PartitionCopy),
    ("batch-format", NativeToolAction::BatchFormat),
    ("storage-driver", NativeToolAction::ImportStorageDriver),
    ("quick-partition", NativeToolAction::QuickPartition),
    ("appx", NativeToolAction::RemoveAppx),
    ("driver-transfer", NativeToolAction::DriverBackupRestore),
    ("repair-boot", NativeToolAction::RepairBoot),
    ("network-info", NativeToolAction::NetworkInformation),
    ("software-list", NativeToolAction::SoftwareList),
    ("time-sync", NativeToolAction::TimeSynchronization),
    ("ghost", NativeToolAction::RunGhost),
    ("gho-password", NativeToolAction::ReadGhoPassword),
    ("reset-network", NativeToolAction::ResetNetwork),
    ("space-sniffer", NativeToolAction::RunSpaceSniffer),
    ("verify-image", NativeToolAction::VerifyImage),
    ("bitlocker", NativeToolAction::ManageBitLocker),
    ("file-hash", NativeToolAction::VerifyFileHash),
    ("reset-password", NativeToolAction::ResetPassword),
    ("expand-c", NativeToolAction::ExpandC),
    ("hardware-inspect", NativeToolAction::HardwareInspector),
    ("pe-maintenance", NativeToolAction::EnterPeMaintenance),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInvocation {
    pub name: String,
    pub action: String,
    pub options: BTreeMap<String, String>,
    pub flags: Vec<String>,
}

impl ToolInvocation {
    pub fn is_live(&self) -> bool {
        self.action == "run" || self.action == "remove"
    }

    pub fn requires_administrator(&self) -> bool {
        self.is_live() || (self.name == "bitlocker" && self.action == "read-key")
    }
}

pub fn parse(args: &[String]) -> Result<ToolInvocation> {
    let name = args
        .get(2)
        .ok_or_else(|| anyhow!("expected tool list or tool <name> <action>"))?;
    if name == "list" {
        if args.len() != 3 {
            return Err(anyhow!("tool list does not accept additional arguments"));
        }
        return Ok(ToolInvocation {
            name: name.clone(),
            action: "list".into(),
            options: BTreeMap::new(),
            flags: Vec::new(),
        });
    }
    if !TOOL_NAMES.iter().any(|(candidate, _)| candidate == name) {
        return Err(anyhow!("unknown toolbox command {name:?}; use 'tool list'"));
    }
    let action = args
        .get(3)
        .ok_or_else(|| anyhow!("tool {name} requires an action"))?;
    let (options, flags) = collect_options(&args[4..])?;
    validate_grammar(name, action, &options, &flags)?;
    Ok(ToolInvocation {
        name: name.clone(),
        action: action.clone(),
        options,
        flags,
    })
}

fn validate_grammar(
    name: &str,
    action: &str,
    options: &BTreeMap<String, String>,
    flags: &[String],
) -> Result<()> {
    let (actions, allowed_options, allowed_flags): (&[&str], &[&str], &[&str]) = match name {
        "nvidia-driver" => (&["inventory", "plan", "remove"], &["target"], &["yes"]),
        "partition-copy" => (
            &["inventory", "plan", "run"],
            &["source", "target"],
            &["yes"],
        ),
        "batch-format" => (
            &["inventory", "plan", "run"],
            &["drives", "file-system", "label"],
            &["yes"],
        ),
        "storage-driver" => (&["inventory", "plan", "run"], &["target"], &["yes"]),
        "quick-partition" => (
            &["inventory", "plan", "run"],
            &["disk-number", "style", "layout-file"],
            &["yes"],
        ),
        "appx" => (
            &["inventory", "plan", "run"],
            &["target", "packages-file"],
            &["yes"],
        ),
        "driver-transfer" => (
            &["inventory", "plan", "run"],
            &["mode", "target", "directory"],
            &["yes"],
        ),
        "repair-boot" => (&["inventory", "plan", "run"], &["target"], &["yes"]),
        "network-info" | "software-list" | "hardware-inspect" => (&["inspect"], &[], &[]),
        "time-sync" | "ghost" | "reset-network" | "space-sniffer" => {
            (&["plan", "run"], &[], &["yes"])
        }
        "gho-password" => (&["read"], &["path"], &["show-secret"]),
        "verify-image" => (&["inspect"], &["path"], &[]),
        "bitlocker" => (
            &["inventory", "read-key", "plan", "run"],
            &["volume", "operation"],
            &["yes", "secret-stdin", "show-secret"],
        ),
        "file-hash" => (&["inspect"], &["path", "expected"], &[]),
        "reset-password" => (
            &["inventory", "plan", "run"],
            &["target", "account"],
            &["yes"],
        ),
        "expand-c" => (&["analyze", "plan", "run"], &["target-size-mb"], &["yes"]),
        "pe-maintenance" => (&["plan", "run"], &[], &["yes"]),
        _ => unreachable!("tool name was validated"),
    };
    if !actions.contains(&action) {
        return Err(anyhow!("tool {name} expects action {}", actions.join("|")));
    }
    if let Some(option) = options
        .keys()
        .find(|option| !allowed_options.contains(&option.as_str()))
    {
        return Err(anyhow!("unrecognized option --{option} for tool {name}"));
    }
    if let Some(flag) = flags
        .iter()
        .find(|flag| !allowed_flags.contains(&flag.as_str()))
    {
        return Err(anyhow!("unrecognized flag --{flag} for tool {name}"));
    }
    if action != "run" && action != "remove" && flags.contains(&"yes".to_owned()) {
        return Err(anyhow!(
            "--yes is accepted only by a live run/remove action"
        ));
    }
    if (action == "run" || action == "remove") && !flags.contains(&"yes".to_owned()) {
        return Err(anyhow!("tool {name} {action} requires explicit --yes"));
    }
    Ok(())
}

fn collect_options(args: &[String]) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut options = BTreeMap::new();
    let mut flags = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = args[index]
            .strip_prefix("--")
            .ok_or_else(|| anyhow!("unexpected positional argument {:?}", args[index]))?;
        if token.is_empty() {
            return Err(anyhow!("empty option name"));
        }
        if index + 1 < args.len() && !args[index + 1].starts_with("--") {
            if options
                .insert(token.to_owned(), args[index + 1].clone())
                .is_some()
            {
                return Err(anyhow!("duplicate option --{token}"));
            }
            index += 2;
        } else {
            if flags.iter().any(|flag| flag == token) {
                return Err(anyhow!("duplicate flag --{token}"));
            }
            flags.push(token.to_owned());
            index += 1;
        }
    }
    Ok((options, flags))
}

fn required<'a>(invocation: &'a ToolInvocation, name: &str) -> Result<&'a str> {
    invocation
        .options
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("--{name} is required"))
}

fn optional<'a>(invocation: &'a ToolInvocation, name: &str) -> Option<&'a str> {
    invocation
        .options
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn flag(invocation: &ToolInvocation, name: &str) -> bool {
    invocation.flags.iter().any(|flag| flag == name)
}

fn current_environment() -> ToolEnvironment {
    if super::disk::DiskManager::is_pe_environment() {
        ToolEnvironment::Pe
    } else {
        ToolEnvironment::Desktop
    }
}

fn ensure_environment(action: NativeToolAction) -> Result<()> {
    let plan = plan_tool(action);
    if plan.is_supported(current_environment()) {
        Ok(())
    } else {
        Err(anyhow!(
            "tool {action:?} is unavailable in the current environment"
        ))
    }
}

fn confirmed(action: NativeToolAction) -> Result<super::native_tool_executor::ConfirmedToolPlan> {
    ensure_environment(action)?;
    match plan_execution(ToolExecutionRequest::NativeAction {
        action,
        confirmed: true,
    }) {
        ToolExecutionPlan::Mutating(plan) => Ok(plan),
        other => Err(anyhow!(
            "tool action did not produce a mutating plan: {other:?}"
        )),
    }
}

fn external(action: NativeToolAction) -> Result<super::native_tool_executor::ExternalToolPlan> {
    ensure_environment(action)?;
    match plan_execution(ToolExecutionRequest::NativeAction {
        action,
        confirmed: true,
    }) {
        ToolExecutionPlan::External(plan) => Ok(plan),
        other => Err(anyhow!(
            "tool action did not produce an external plan: {other:?}"
        )),
    }
}

fn plan_json(action: NativeToolAction, detail: Value) -> Result<Value> {
    ensure_environment(action)?;
    let plan = plan_tool(action);
    Ok(json!({
        "tool": tool_name(action),
        "environment": format!("{:?}", current_environment()).to_ascii_lowercase(),
        "safety": safety_name(plan.safety),
        "requires_confirmation": plan.safety.requires_explicit_execution(),
        "detail": detail,
    }))
}

fn tool_name(action: NativeToolAction) -> &'static str {
    TOOL_NAMES
        .iter()
        .find(|(_, candidate)| *candidate == action)
        .map(|(name, _)| *name)
        .expect("every native tool action has one public CLI name")
}

const fn safety_name(safety: ToolSafetyClass) -> &'static str {
    match safety {
        ToolSafetyClass::ReadOnly => "read_only",
        ToolSafetyClass::SensitiveRead => "sensitive_read",
        ToolSafetyClass::SystemMutation => "system_mutation",
        ToolSafetyClass::StorageMutation => "storage_mutation",
        ToolSafetyClass::DestructiveStorage => "destructive_storage",
        ToolSafetyClass::SecurityMutation => "security_mutation",
        ToolSafetyClass::ExternalProgram => "external_program",
    }
}

pub fn help_text() -> &'static str {
    "tool list\n  tool nvidia-driver inventory|plan|remove [--target current|X:] [--yes]\n  tool partition-copy inventory|plan|run --source X: --target Y: [--yes]\n  tool batch-format inventory|plan|run --drives X:,Y: --file-system NTFS|FAT32|exFAT [--label text] [--yes]\n  tool storage-driver inventory|plan|run --target X: [--yes]\n  tool quick-partition inventory|plan|run --disk-number N --style GPT|MBR --layout-file <file> [--yes]\n  tool appx inventory --target current|X:\n  tool appx plan|run --target current|X: --packages-file <file> [--yes]\n  tool driver-transfer inventory|plan|run --mode backup|restore --target current|X: --directory <dir> [--yes]\n  tool repair-boot inventory|plan|run --target X: [--yes]\n  tool network-info inspect\n  tool software-list inspect\n  tool time-sync plan|run [--yes]\n  tool ghost plan|run [--yes]\n  tool gho-password read --path <file> [--show-secret]\n  tool reset-network plan|run [--yes]\n  tool space-sniffer plan|run [--yes]\n  tool verify-image inspect --path <file>\n  tool bitlocker inventory\n  tool bitlocker read-key --volume X: [--show-secret]\n  tool bitlocker plan|run --volume X: --operation unlock-password|unlock-recovery|decrypt|suspend|resume [--secret-stdin] [--yes]\n  tool file-hash inspect --path <file> [--expected <sha256>]\n  tool reset-password inventory --target current|X:\n  tool reset-password plan|run --target current|X: --account <name> [--yes]\n  tool expand-c analyze\n  tool expand-c plan|run --target-size-mb N [--yes]\n  tool hardware-inspect inspect\n  tool pe-maintenance plan|run [--yes]\n\nAll output is JSON. Live mutations require an already elevated console and explicit --yes. BitLocker unlock secrets are accepted only through --secret-stdin. Sensitive read commands redact secrets unless --show-secret is explicit."
}

pub fn execute(invocation: &ToolInvocation) -> Result<Value> {
    match (invocation.name.as_str(), invocation.action.as_str()) {
        ("list", "list") => list_tools(),
        ("network-info", "inspect") => execute_read_only(
            NativeToolAction::NetworkInformation,
            ReadOnlyToolRequest::NetworkInformation,
            false,
        ),
        ("software-list", "inspect") => execute_read_only(
            NativeToolAction::SoftwareList,
            ReadOnlyToolRequest::InstalledSoftware,
            false,
        ),
        ("gho-password", "read") => execute_read_only(
            NativeToolAction::ReadGhoPassword,
            ReadOnlyToolRequest::GhoPassword {
                path: required(invocation, "path")?.to_owned(),
            },
            flag(invocation, "show-secret"),
        ),
        ("verify-image", "inspect") => execute_read_only(
            NativeToolAction::VerifyImage,
            ReadOnlyToolRequest::VerifyImage {
                path: required(invocation, "path")?.to_owned(),
            },
            false,
        ),
        ("file-hash", "inspect") => execute_read_only(
            NativeToolAction::VerifyFileHash,
            ReadOnlyToolRequest::Sha256 {
                path: required(invocation, "path")?.to_owned(),
                expected: optional(invocation, "expected")
                    .unwrap_or_default()
                    .to_owned(),
            },
            false,
        ),
        ("hardware-inspect", "inspect") => hardware_inspect(),
        ("nvidia-driver", "inventory") => nvidia_inventory(),
        ("partition-copy", "inventory") => partition_copy_inventory(),
        ("batch-format", "inventory") => batch_format_inventory(),
        ("storage-driver", "inventory") => windows_target_inventory(false),
        ("quick-partition", "inventory") => quick_partition_inventory(),
        ("appx", "inventory") => appx_inventory(invocation),
        ("driver-transfer", "inventory") => windows_target_inventory(true),
        ("repair-boot", "inventory") => boot_repair_inventory(),
        ("bitlocker", "inventory") => bitlocker_inventory(),
        ("bitlocker", "read-key") => bitlocker_read_key(invocation),
        ("reset-password", "inventory") => password_inventory(invocation),
        ("expand-c", "analyze") => expand_c_analysis(),
        ("nvidia-driver", "plan") => nvidia_plan(invocation),
        ("nvidia-driver", "remove") => nvidia_run(invocation),
        ("partition-copy", "plan") => partition_copy_plan(invocation),
        ("partition-copy", "run") => partition_copy_run(invocation),
        ("batch-format", "plan") => batch_format_plan(invocation),
        ("batch-format", "run") => batch_format_run(invocation),
        ("storage-driver", "plan") => storage_driver_plan(invocation),
        ("storage-driver", "run") => storage_driver_run(invocation),
        ("quick-partition", "plan") => quick_partition_plan(invocation),
        ("quick-partition", "run") => quick_partition_run(invocation),
        ("appx", "plan") => appx_plan(invocation),
        ("appx", "run") => appx_run(invocation),
        ("driver-transfer", "plan") => driver_transfer_plan(invocation),
        ("driver-transfer", "run") => driver_transfer_run(invocation),
        ("repair-boot", "plan") => repair_boot_plan(invocation),
        ("repair-boot", "run") => repair_boot_run(invocation),
        ("time-sync", "plan") => simple_plan(NativeToolAction::TimeSynchronization),
        ("time-sync", "run") => simple_backend_run(
            super::native_tool_backend::NativeToolBackendRequest::SynchronizeTime(confirmed(
                NativeToolAction::TimeSynchronization,
            )?),
        ),
        ("ghost", "plan") => simple_plan(NativeToolAction::RunGhost),
        ("ghost", "run") => external_run(NativeToolAction::RunGhost),
        ("reset-network", "plan") => simple_plan(NativeToolAction::ResetNetwork),
        ("reset-network", "run") => simple_backend_run(
            super::native_tool_backend::NativeToolBackendRequest::ResetNetwork(confirmed(
                NativeToolAction::ResetNetwork,
            )?),
        ),
        ("space-sniffer", "plan") => simple_plan(NativeToolAction::RunSpaceSniffer),
        ("space-sniffer", "run") => external_run(NativeToolAction::RunSpaceSniffer),
        ("bitlocker", "plan") => bitlocker_plan(invocation),
        ("bitlocker", "run") => bitlocker_run(invocation),
        ("reset-password", "plan") => password_plan(invocation),
        ("reset-password", "run") => password_run(invocation),
        ("expand-c", "plan") => expand_c_plan(invocation),
        ("expand-c", "run") => expand_c_run(invocation),
        ("pe-maintenance", "plan") => pe_maintenance_plan(),
        ("pe-maintenance", "run") => pe_maintenance_run(),
        _ => Err(anyhow!("unsupported toolbox command")),
    }
}

fn list_tools() -> Result<Value> {
    let environment = current_environment();
    let maintenance_enabled =
        super::app_config::AppConfig::load_strict()?.pe_maintenance_entry_enabled;
    Ok(json!({
        "environment": format!("{environment:?}").to_ascii_lowercase(),
        "tools": TOOL_NAMES.iter().map(|(name, action)| {
            let plan = plan_tool(*action);
            json!({
                "name": name,
                "action": format!("{action:?}"),
                "safety": safety_name(plan.safety),
                "available": plan.is_supported(environment)
                    && (*action != NativeToolAction::EnterPeMaintenance || maintenance_enabled),
                "usage": tool_usage(name),
            })
        }).collect::<Vec<_>>(),
    }))
}

fn tool_usage(name: &str) -> &'static str {
    match name {
        "nvidia-driver" => "inventory | plan/remove [--target current|X:] [--yes]",
        "partition-copy" => "inventory | plan/run --source X: --target Y: [--yes]",
        "batch-format" => "inventory | plan/run --drives X:,Y: --file-system <fs> [--label <text>] [--yes]",
        "storage-driver" => "inventory | plan/run --target X: [--yes]",
        "quick-partition" => "inventory | plan/run --disk-number N --style GPT|MBR --layout-file <file> [--yes]",
        "appx" => "inventory --target current|X: | plan/run --target current|X: --packages-file <file> [--yes]",
        "driver-transfer" => "inventory | plan/run --mode backup|restore --target current|X: --directory <dir> [--yes]",
        "repair-boot" => "inventory | plan/run --target X: [--yes]",
        "network-info" | "software-list" | "hardware-inspect" => "inspect",
        "time-sync" | "ghost" | "reset-network" | "space-sniffer" => "plan | run --yes",
        "gho-password" => "read --path <file> [--show-secret]",
        "verify-image" => "inspect --path <file>",
        "bitlocker" => "inventory | read-key --volume X: [--show-secret] | plan/run --volume X: --operation <operation> [--secret-stdin] [--yes]",
        "file-hash" => "inspect --path <file> [--expected <sha256>]",
        "reset-password" => "inventory --target current|X: | plan/run --target current|X: --account <name> [--yes]",
        "expand-c" => "analyze | plan/run --target-size-mb N [--yes]",
        "pe-maintenance" => "plan | run --yes",
        _ => "",
    }
}

fn execute_read_only(
    action: NativeToolAction,
    request: ReadOnlyToolRequest,
    show_secret: bool,
) -> Result<Value> {
    ensure_environment(action)?;
    let plan = plan_execution(ToolExecutionRequest::ReadOnly(request));
    let mut reporter = |event| {
        let super::native_tool_executor::ToolExecutionEvent::Progress { percentage, detail } =
            event;
        super::cli::emit_progress(json!({
            "event":"tool_progress",
            "tool":tool_name(action),
            "percentage":percentage,
            "detail":detail,
        }));
    };
    let result =
        super::native_tool_executor::NativeToolExecutor::execute_read_only(&plan, &mut reporter)
            .map_err(|error| anyhow!(error))?;
    read_only_json(result, show_secret)
}

fn read_only_json(result: ReadOnlyToolResult, show_secret: bool) -> Result<Value> {
    Ok(match result {
        ReadOnlyToolResult::Sha256(value) => json!({
            "path":value.path,"file_size":value.file_size,"sha256":value.sha256,
            "expected":value.expected,"matched":value.matched,
        }),
        ReadOnlyToolResult::GhoPassword(value) => json!({
            "path":value.path,"valid":value.valid,"has_password":value.has_password,
            "password":if show_secret { value.password } else { None },
            "password_redacted":value.has_password && !show_secret,
            "password_length":value.password_length,"error":value.error,
        }),
        ReadOnlyToolResult::ImageVerification(value) => json!({
            "path":value.path,"image_type":value.image_type,"status":value.status,
            "valid":value.valid,"file_size":value.file_size,"image_count":value.image_count,
            "part_count":value.part_count,"message":value.message,"details":value.details,
        }),
        ReadOnlyToolResult::InstalledSoftware(values) => json!({
            "items":values.into_iter().map(|value| json!({
                "name":value.name,"version":value.version,"publisher":value.publisher,
                "install_location":value.install_location,
            })).collect::<Vec<_>>()
        }),
        ReadOnlyToolResult::NetworkInformation(values) => json!({
            "adapters":values.into_iter().map(|value| json!({
                "name":value.name,"description":value.description,"mac_address":value.mac_address,
                "ip_addresses":value.ip_addresses,"adapter_type":value.adapter_type,
                "status":value.status,"speed":value.speed,
            })).collect::<Vec<_>>()
        }),
    })
}

fn nvidia_inventory() -> Result<Value> {
    ensure_environment(NativeToolAction::NvidiaDriverRemoval)?;
    let report =
        super::native_nvidia_removal::load_hardware_report().map_err(|error| anyhow!(error))?;
    Ok(json!({
        "nvidia_device_count":report.nvidia_device_count,
        "rows":report.rows.into_iter().map(|row| json!({
            "item":row.item,"value":row.value,"is_nvidia":row.is_nvidia,
        })).collect::<Vec<_>>()
    }))
}

fn partition_copy_inventory() -> Result<Value> {
    ensure_environment(NativeToolAction::PartitionCopy)?;
    let items = super::native_partition_copy::read_inventory().map_err(|error| anyhow!(error))?;
    Ok(json!({"volumes":items.into_iter().map(|item| json!({
        "drive":item.drive,"label":item.label,"total_size_mb":item.total_size_mb,
        "used_size_mb":item.used_size_mb,"free_size_mb":item.free_size_mb,
        "has_system":item.has_system,
    })).collect::<Vec<_>>()}))
}

fn batch_format_inventory() -> Result<Value> {
    ensure_environment(NativeToolAction::BatchFormat)?;
    let items = super::native_batch_format::inventory_current().map_err(|error| anyhow!(error))?;
    Ok(json!({"volumes":items.into_iter().map(|item| json!({
        "drive":item.drive,"label":item.label,"file_system":item.file_system,
        "total_size_mb":item.total_size_mb,"free_size_mb":item.free_size_mb,
    })).collect::<Vec<_>>()}))
}

fn current_partitions() -> Result<Vec<super::disk::Partition>> {
    super::disk::DiskManager::get_partitions().map_err(|error| anyhow!(error))
}

fn windows_target_inventory(include_current: bool) -> Result<Value> {
    let items =
        super::native_tool_inventory::load_windows_targets(&current_partitions()?, include_current)
            .map_err(|error| anyhow!(error))?;
    Ok(json!({"targets":items.into_iter().map(|item| json!({
        "value":item.value,"label":item.label,
    })).collect::<Vec<_>>()}))
}

fn quick_partition_inventory() -> Result<Value> {
    ensure_environment(NativeToolAction::QuickPartition)?;
    let disks = super::quick_partition::get_physical_disks();
    Ok(json!({"disks":disks.iter().map(|disk| {
        let (safe, reason) = super::quick_partition::can_safely_partition(disk);
        json!({
            "disk_number":disk.disk_number,"model":disk.model,"size_bytes":disk.size_bytes,
            "partition_style":format!("{:?}",disk.partition_style),"initialized":disk.is_initialized,
            "unallocated_bytes":disk.unallocated_bytes,"safe_to_partition":safe,"reason":reason,
            "default_gpt_layout":super::native_quick_partition::format_layouts(
                &super::native_quick_partition::default_layouts(super::disk::PartitionStyle::GPT,disk.size_bytes)),
            "default_mbr_layout":super::native_quick_partition::format_layouts(
                &super::native_quick_partition::default_layouts(super::disk::PartitionStyle::MBR,disk.size_bytes)),
        })
    }).collect::<Vec<_>>() }))
}

fn appx_inventory(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::RemoveAppx)?;
    let target = required(invocation, "target")?;
    let inventory_target = inventory_target_value(target)?;
    let items = super::native_tool_inventory::load_dynamic(
        super::native_tool_inventory::DynamicInventoryKind::RemoveAppxPackages,
        &inventory_target,
    )
    .map_err(|error| anyhow!(error))?;
    Ok(
        json!({"target":target,"packages":items.into_iter().map(|item| json!({
        "package_name":item.value,"display_name":item.label,
    })).collect::<Vec<_>>()}),
    )
}

fn boot_repair_inventory() -> Result<Value> {
    ensure_environment(NativeToolAction::RepairBoot)?;
    let targets = super::native_tool_inventory::load_boot_repair_targets(&current_partitions()?)
        .map_err(|error| anyhow!(error))?;
    Ok(json!({"targets":targets.into_iter().map(|target| json!({
        "partition":target.partition,"windows_version":target.windows_version,
        "architecture":target.architecture,
    })).collect::<Vec<_>>()}))
}

fn bitlocker_inventory() -> Result<Value> {
    ensure_environment(NativeToolAction::ManageBitLocker)?;
    let volumes =
        super::native_bitlocker_manage::read_inventory().map_err(|error| anyhow!(error))?;
    Ok(json!({"volumes":volumes.into_iter().map(bitlocker_volume_json).collect::<Vec<_>>()}))
}

fn bitlocker_volume_json(volume: super::native_bitlocker_manage::BitLockerManageVolume) -> Value {
    json!({
        "drive":volume.drive,"label":volume.label,"total_size_mb":volume.total_size_mb,
        "status":volume.status.as_str(),"protection_method":volume.protection_method,
        "encryption_percentage":volume.encryption_percentage,
    })
}

fn password_inventory(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ResetPassword)?;
    let target = password_target(required(invocation, "target")?)?;
    let accounts = super::native_password_reset::load_password_reset_accounts(&target)
        .map_err(|error| anyhow!(error))?;
    Ok(
        json!({"target":password_target_json(&target),"accounts":accounts.into_iter().map(|account| json!({
        "username":account.username,"disabled":account.disabled,
    })).collect::<Vec<_>>()}),
    )
}

fn expand_c_analysis() -> Result<Value> {
    ensure_environment(NativeToolAction::ExpandC)?;
    let value =
        super::native_expand_c_controller::analyze_expand_c().map_err(|error| anyhow!(error))?;
    Ok(expand_analysis_json(&value))
}

fn expand_analysis_json(value: &super::native_expand_c_controller::NativeExpandCAnalysis) -> Value {
    json!({
        "found":value.found,"partition_number":value.partition_number,
        "current_size_mb":value.current_size_mb,"used_mb":value.used_mb,"free_mb":value.free_mb,
        "max_size_mb":value.max_size_mb,"no_move_max_mb":value.no_move_max_mb,
        "can_expand":value.can_expand,"reason":value.reason,
        "disk_number":value.disk.as_ref().map(|disk| disk.disk_number),
    })
}

fn simple_plan(action: NativeToolAction) -> Result<Value> {
    plan_json(action, json!({}))
}

fn nvidia_target(value: Option<&str>) -> Result<super::native_nvidia_removal::NvidiaRemovalTarget> {
    match value.unwrap_or("current").trim() {
        value if value.eq_ignore_ascii_case("current") => {
            Ok(super::native_nvidia_removal::NvidiaRemovalTarget::CurrentSystem)
        }
        value => Ok(
            super::native_nvidia_removal::NvidiaRemovalTarget::OfflineWindows(canonical_drive(
                value,
            )?),
        ),
    }
}

fn nvidia_plan(invocation: &ToolInvocation) -> Result<Value> {
    let target = nvidia_target(optional(invocation, "target"))?;
    super::native_nvidia_removal::validate_request(
        &super::native_nvidia_removal::NvidiaRemovalRequest {
            target: target.clone(),
        },
    )
    .map_err(|error| anyhow!(error))?;
    if let super::native_nvidia_removal::NvidiaRemovalTarget::OfflineWindows(root) = &target {
        require_detected_windows_target(root, false)?;
    }
    plan_json(
        NativeToolAction::NvidiaDriverRemoval,
        json!({
            "target":nvidia_target_json(&target),
            "scope":super::native_nvidia_removal::removal_scope(&target).map_err(|error| anyhow!(error))?,
        }),
    )
}

fn nvidia_run(invocation: &ToolInvocation) -> Result<Value> {
    let target = nvidia_target(optional(invocation, "target"))?;
    if let super::native_nvidia_removal::NvidiaRemovalTarget::OfflineWindows(root) = &target {
        require_detected_windows_target(root, false)?;
    }
    let request = super::native_nvidia_removal::NvidiaRemovalRequest { target };
    let backend = super::native_nvidia_removal::build_backend_request(
        &request,
        confirmed(NativeToolAction::NvidiaDriverRemoval)?,
    )
    .map_err(|error| anyhow!(error))?;
    simple_backend_run(backend)
}

fn nvidia_target_json(target: &super::native_nvidia_removal::NvidiaRemovalTarget) -> String {
    match target {
        super::native_nvidia_removal::NvidiaRemovalTarget::CurrentSystem => "current".into(),
        super::native_nvidia_removal::NvidiaRemovalTarget::OfflineWindows(root) => root.clone(),
    }
}

fn partition_copy_request(
    invocation: &ToolInvocation,
) -> Result<super::native_partition_copy::PartitionCopyRequest> {
    Ok(super::native_partition_copy::PartitionCopyRequest {
        source: canonical_drive(required(invocation, "source")?)?,
        target: canonical_drive(required(invocation, "target")?)?,
    })
}

fn partition_copy_plan(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::PartitionCopy)?;
    let request = partition_copy_request(invocation)?;
    let plan =
        super::native_partition_copy::validate_current(&request).map_err(|error| anyhow!(error))?;
    plan_json(
        NativeToolAction::PartitionCopy,
        json!({"source":plan.source(),"target":plan.target(),"resume":plan.resume()}),
    )
}

fn partition_copy_run(invocation: &ToolInvocation) -> Result<Value> {
    let request = partition_copy_request(invocation)?;
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::PartitionCopy {
            plan: confirmed(NativeToolAction::PartitionCopy)?,
            request,
        },
    )
}

fn batch_format_request(
    invocation: &ToolInvocation,
) -> Result<super::native_batch_format::BatchFormatRequest> {
    let drives = required(invocation, "drives")?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(canonical_drive)
        .collect::<Result<Vec<_>>>()?;
    Ok(super::native_batch_format::BatchFormatRequest {
        drives,
        file_system: required(invocation, "file-system")?.to_owned(),
        volume_label: optional(invocation, "label").unwrap_or_default().to_owned(),
    })
}

fn batch_format_plan(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::BatchFormat)?;
    let request = batch_format_request(invocation)?;
    let plan =
        super::native_batch_format::validate_current(&request).map_err(|error| anyhow!(error))?;
    plan_json(
        NativeToolAction::BatchFormat,
        json!({
            "drives":plan.drives().collect::<Vec<_>>(),
            "file_system":request.file_system,"volume_label":request.volume_label,
        }),
    )
}

fn batch_format_run(invocation: &ToolInvocation) -> Result<Value> {
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::BatchFormat {
            plan: confirmed(NativeToolAction::BatchFormat)?,
            request: batch_format_request(invocation)?,
        },
    )
}

struct PreparedStorageDriver {
    target: String,
    directory: String,
}

fn prepare_storage_driver(invocation: &ToolInvocation) -> Result<PreparedStorageDriver> {
    ensure_environment(NativeToolAction::ImportStorageDriver)?;
    let target = canonical_drive(required(invocation, "target")?)?;
    let partitions = current_partitions()?;
    let targets = super::native_tool_inventory::load_windows_targets(&partitions, true)
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .skip(1)
        .map(|entry| {
            super::native_storage_driver::StorageDriverTarget::new(entry.value, entry.label)
        })
        .collect::<Vec<_>>();
    let system = lr_core::windows_storage::current_windows_drive_letter()
        .map(|letter| format!("{letter}:"))
        .map_err(|error| anyhow!(error))?;
    let plan = super::native_storage_driver::prepare_current(
        &super::native_storage_driver::StorageDriverImportRequest { target },
        &targets,
        &system,
    )
    .map_err(|error| anyhow!(error))?;
    let hardware_ids =
        lr_core::driver::list_present_hardware_ids().map_err(|error| anyhow!(error))?;
    let packages = lr_core::storage_driver_match::select_builtin_storage_driver_packages(
        hardware_ids.iter().map(String::as_str),
    )
    .map_err(|error| anyhow!(error))?;
    let [package] = packages.as_slice() else {
        return Err(anyhow!(
            "no unique Intel VMD package matches the current hardware"
        ));
    };
    let directory = plan.driver_directory().join(package.directory_name());
    let verified =
        lr_core::storage_driver_match::verify_builtin_storage_driver_package(*package, &directory)
            .map_err(|error| anyhow!(error))?;
    Ok(PreparedStorageDriver {
        target: plan.target().to_owned(),
        directory: verified.directory().to_string_lossy().into_owned(),
    })
}

fn storage_driver_plan(invocation: &ToolInvocation) -> Result<Value> {
    let prepared = prepare_storage_driver(invocation)?;
    plan_json(
        NativeToolAction::ImportStorageDriver,
        json!({"target":prepared.target,"packaged_driver_directory":prepared.directory}),
    )
}

fn storage_driver_run(invocation: &ToolInvocation) -> Result<Value> {
    let prepared = prepare_storage_driver(invocation)?;
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::ImportStorageDriver {
            plan: confirmed(NativeToolAction::ImportStorageDriver)?,
            target: prepared.target,
            driver_directory: prepared.directory,
        },
    )
}

fn quick_partition_request(
    invocation: &ToolInvocation,
) -> Result<super::native_quick_partition::QuickPartitionRequest> {
    ensure_environment(NativeToolAction::QuickPartition)?;
    let disk_number = required(invocation, "disk-number")?
        .parse::<u32>()
        .map_err(|_| anyhow!("--disk-number must be an unsigned integer"))?;
    let style = match required(invocation, "style")?
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "GPT" => super::disk::PartitionStyle::GPT,
        "MBR" => super::disk::PartitionStyle::MBR,
        _ => return Err(anyhow!("--style must be GPT or MBR")),
    };
    let layout = read_plain_text(Path::new(required(invocation, "layout-file")?), 64 * 1024)?;
    let layouts =
        super::native_quick_partition::parse_layouts(&layout).map_err(|error| anyhow!(error))?;
    let disks = super::quick_partition::get_physical_disks();
    let disk = disks
        .iter()
        .find(|disk| disk.disk_number == disk_number)
        .ok_or_else(|| anyhow!("disk {disk_number} is not present"))?;
    let request = super::native_quick_partition::QuickPartitionRequest {
        disk: super::native_quick_partition::DiskFingerprint::from(disk),
        partition_style: style,
        layouts,
    };
    super::native_quick_partition::validate_request(&request).map_err(|error| anyhow!(error))?;
    Ok(request)
}

fn quick_partition_plan(invocation: &ToolInvocation) -> Result<Value> {
    let request = quick_partition_request(invocation)?;
    let disks = super::quick_partition::get_physical_disks();
    let disk = super::native_quick_partition::verify_current_disk(&request, &disks)
        .map_err(|error| anyhow!(error))?;
    let (safe, reason) = super::quick_partition::can_safely_partition(disk);
    if !safe {
        return Err(anyhow!(reason));
    }
    plan_json(
        NativeToolAction::QuickPartition,
        json!({
            "disk_number":request.disk.disk_number,"model":request.disk.model,
            "size_bytes":request.disk.size_bytes,"partition_style":format!("{:?}",request.partition_style),
            "layouts":super::native_quick_partition::format_layouts(&request.layouts),
        }),
    )
}

fn quick_partition_run(invocation: &ToolInvocation) -> Result<Value> {
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::QuickPartition {
            plan: confirmed(NativeToolAction::QuickPartition)?,
            request: quick_partition_request(invocation)?,
        },
    )
}

fn appx_request(invocation: &ToolInvocation) -> Result<super::native_appx::RemoveAppxRequest> {
    let target = appx_target(required(invocation, "target")?)?;
    let packages = read_plain_lines(
        Path::new(required(invocation, "packages-file")?),
        1024 * 1024,
        4096,
    )?;
    let request = super::native_appx::RemoveAppxRequest { target, packages };
    super::native_appx::validate_request(&request).map_err(|error| anyhow!(error))?;
    Ok(request)
}

fn appx_plan(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::RemoveAppx)?;
    let request = appx_request(invocation)?;
    let inventory_target = match &request.target {
        super::native_appx::AppxTarget::CurrentSystem => "__CURRENT__".to_owned(),
        super::native_appx::AppxTarget::OfflineWindows(root) => root.clone(),
    };
    let fresh = super::native_tool_inventory::load_dynamic(
        super::native_tool_inventory::DynamicInventoryKind::RemoveAppxPackages,
        &inventory_target,
    )
    .map_err(|error| anyhow!(error))?;
    for package in &request.packages {
        if !fresh
            .iter()
            .any(|item| item.value.eq_ignore_ascii_case(package.trim()))
        {
            return Err(anyhow!(
                "APPX package is not in the fresh inventory: {package:?}"
            ));
        }
    }
    plan_json(
        NativeToolAction::RemoveAppx,
        json!({"target":appx_target_json(&request.target),"packages":request.packages}),
    )
}

fn appx_run(invocation: &ToolInvocation) -> Result<Value> {
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::RemoveAppx {
            plan: confirmed(NativeToolAction::RemoveAppx)?,
            request: appx_request(invocation)?,
        },
    )
}

struct PreparedDriverTransfer {
    mode: super::native_tool_backend::DriverTransferMode,
    system_partition: Option<String>,
    directory: String,
}

fn prepare_driver_transfer(invocation: &ToolInvocation) -> Result<PreparedDriverTransfer> {
    ensure_environment(NativeToolAction::DriverBackupRestore)?;
    let mode = match required(invocation, "mode")?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "backup" => super::native_tool_backend::DriverTransferMode::Backup,
        "restore" => super::native_tool_backend::DriverTransferMode::Restore,
        _ => return Err(anyhow!("--mode must be backup or restore")),
    };
    let target = required(invocation, "target")?.trim();
    let system_partition = if target.eq_ignore_ascii_case("current") {
        if mode == super::native_tool_backend::DriverTransferMode::Restore {
            return Err(anyhow!("driver restore requires an offline Windows target"));
        }
        None
    } else {
        let root = canonical_drive(target)?;
        require_detected_windows_target(&root, false)?;
        Some(root)
    };
    let directory = required(invocation, "directory")?.trim().to_owned();
    if mode == super::native_tool_backend::DriverTransferMode::Restore
        && !Path::new(&directory).is_dir()
    {
        return Err(anyhow!("driver restore source directory does not exist"));
    }
    Ok(PreparedDriverTransfer {
        mode,
        system_partition,
        directory,
    })
}

fn driver_transfer_plan(invocation: &ToolInvocation) -> Result<Value> {
    let prepared = prepare_driver_transfer(invocation)?;
    plan_json(
        NativeToolAction::DriverBackupRestore,
        json!({
            "mode":format!("{:?}",prepared.mode).to_ascii_lowercase(),
            "target":prepared.system_partition.as_deref().unwrap_or("current"),
            "directory":prepared.directory,
        }),
    )
}

fn driver_transfer_run(invocation: &ToolInvocation) -> Result<Value> {
    let prepared = prepare_driver_transfer(invocation)?;
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::TransferDrivers {
            plan: confirmed(NativeToolAction::DriverBackupRestore)?,
            mode: prepared.mode,
            system_partition: prepared.system_partition,
            directory: prepared.directory,
        },
    )
}

fn prepare_repair_boot(
    invocation: &ToolInvocation,
) -> Result<super::native_tool_backend::NativeToolBackendRequest> {
    ensure_environment(NativeToolAction::RepairBoot)?;
    let targets = super::native_tool_inventory::load_boot_repair_targets(&current_partitions()?)
        .map_err(|error| anyhow!(error))?;
    super::native_boot_repair::build_backend_request(
        confirmed(NativeToolAction::RepairBoot)?,
        &super::native_boot_repair::BootRepairRequest {
            target_partition: required(invocation, "target")?.to_owned(),
        },
        &targets,
    )
    .map_err(|error| anyhow!(error))
}

fn repair_boot_plan(invocation: &ToolInvocation) -> Result<Value> {
    let request = prepare_repair_boot(invocation)?;
    let super::native_tool_backend::NativeToolBackendRequest::RepairBoot { target, .. } = request
    else {
        unreachable!()
    };
    plan_json(
        NativeToolAction::RepairBoot,
        json!({"target":target,"mode":"auto"}),
    )
}

fn repair_boot_run(invocation: &ToolInvocation) -> Result<Value> {
    simple_backend_run(prepare_repair_boot(invocation)?)
}

fn external_run(action: NativeToolAction) -> Result<Value> {
    simple_backend_run(
        super::native_tool_backend::NativeToolBackendRequest::External(external(action)?),
    )
}

fn simple_backend_run(
    request: super::native_tool_backend::NativeToolBackendRequest,
) -> Result<Value> {
    let result = super::native_tool_backend::NativeToolBackend::execute(&request)
        .map_err(|error| anyhow!(error))?;
    Ok(backend_result_json(result))
}

fn backend_result_json(result: super::native_tool_backend::NativeToolBackendResult) -> Value {
    use super::native_tool_backend::NativeToolBackendResult as ResultValue;
    match result {
        ResultValue::ExternalStarted => json!({"started":true}),
        ResultValue::TimeSynchronization {
            success,
            message,
            old_time,
            new_time,
        } => json!({
            "success":success,"message":message,"old_time":old_time,"new_time":new_time,
        }),
        ResultValue::NetworkReset { succeeded, failed } => json!({
            "succeeded":succeeded,"failed":failed,
        }),
        ResultValue::NvidiaRemoval {
            success,
            message,
            needs_reboot,
            uninstalled_count,
            failed_count,
        } => json!({
            "success":success,"message":message,"needs_reboot":needs_reboot,
            "uninstalled_count":uninstalled_count,"failed_count":failed_count,
        }),
        ResultValue::AppxRemoval(value) => json!({"removed":value.removed,"failed":value.failed}),
        ResultValue::BatchFormat(value) => json!({
            "success_count":value.success_count,"fail_count":value.fail_count,
            "volumes":value.volumes.into_iter().map(|volume| json!({
                "drive":volume.drive,"success":volume.success,"message":volume.message,
                "exit_code":volume.exit_code,
            })).collect::<Vec<_>>()
        }),
        ResultValue::PartitionCopy(value) => json!({
            "success":value.success,"partial_success":value.partial_success,"resumed":value.resumed,
            "copied_count":value.copied_count,"skipped_count":value.skipped_count,
            "failed_count":value.failed_count,"total_count":value.total_count,
            "failed_files":value.failed_files,"message":value.message,
        }),
        ResultValue::Completed { message } => json!({"completed":true,"message":message}),
        ResultValue::BitLocker {
            success,
            message,
            error_code,
        } => json!({
            "success":success,"message":message,"error_code":error_code,
        }),
    }
}

fn bitlocker_read_key(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ManageBitLocker)?;
    let volume = canonical_drive(required(invocation, "volume")?)?;
    let secret = Zeroizing::new(
        super::native_bitlocker_manage::read_recovery_key(&volume)
            .map_err(|error| anyhow!(error))?,
    );
    Ok(json!({
        "volume":volume,
        "recovery_key":if flag(invocation,"show-secret") { Some(secret.as_str()) } else { None },
        "recovery_key_redacted":!flag(invocation,"show-secret"),
    }))
}

fn bitlocker_operation(invocation: &ToolInvocation) -> Result<&str> {
    let operation = required(invocation, "operation")?;
    if [
        "unlock-password",
        "unlock-recovery",
        "decrypt",
        "suspend",
        "resume",
    ]
    .contains(&operation)
    {
        Ok(operation)
    } else {
        Err(anyhow!("unsupported BitLocker operation {operation:?}"))
    }
}

fn bitlocker_plan(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ManageBitLocker)?;
    let volume = canonical_drive(required(invocation, "volume")?)?;
    let operation = bitlocker_operation(invocation)?;
    let inventory =
        super::native_bitlocker_manage::read_inventory().map_err(|error| anyhow!(error))?;
    let current = inventory
        .iter()
        .find(|candidate| candidate.drive.eq_ignore_ascii_case(&volume))
        .ok_or_else(|| {
            anyhow!("BitLocker volume is not in the fresh encrypted-volume inventory")
        })?;
    let action = match operation {
        "unlock-password" | "unlock-recovery" => {
            super::native_bitlocker_manage::BitLockerManageAction::Unlock
        }
        "decrypt" => super::native_bitlocker_manage::BitLockerManageAction::Decrypt,
        "suspend" => super::native_bitlocker_manage::BitLockerManageAction::SuspendProtection,
        "resume" => super::native_bitlocker_manage::BitLockerManageAction::ResumeProtection,
        _ => unreachable!(),
    };
    if operation != "unlock-password" && operation != "unlock-recovery" {
        super::native_bitlocker_manage::build_intent(
            &inventory,
            &volume,
            action,
            super::native_bitlocker_manage::BitLockerUnlockMethod::Password,
            String::new(),
        )
        .map_err(|error| anyhow!(error))?;
    } else if !current.status.needs_unlock() {
        return Err(anyhow!("selected BitLocker volume is not locked"));
    }
    plan_json(
        NativeToolAction::ManageBitLocker,
        json!({
            "volume":volume,"operation":operation,"current_status":current.status.as_str(),
            "secret_source":if operation.starts_with("unlock-") { "stdin" } else { "none" },
        }),
    )
}

fn bitlocker_run(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ManageBitLocker)?;
    let volume = canonical_drive(required(invocation, "volume")?)?;
    let operation = bitlocker_operation(invocation)?;
    let inventory =
        super::native_bitlocker_manage::read_inventory().map_err(|error| anyhow!(error))?;
    let (action, unlock_method, secret) = match operation {
        "unlock-password" => (
            super::native_bitlocker_manage::BitLockerManageAction::Unlock,
            super::native_bitlocker_manage::BitLockerUnlockMethod::Password,
            read_secret_stdin(invocation)?,
        ),
        "unlock-recovery" => (
            super::native_bitlocker_manage::BitLockerManageAction::Unlock,
            super::native_bitlocker_manage::BitLockerUnlockMethod::RecoveryKey,
            read_secret_stdin(invocation)?,
        ),
        "decrypt" => (
            super::native_bitlocker_manage::BitLockerManageAction::Decrypt,
            super::native_bitlocker_manage::BitLockerUnlockMethod::Password,
            Zeroizing::new(String::new()),
        ),
        "suspend" => (
            super::native_bitlocker_manage::BitLockerManageAction::SuspendProtection,
            super::native_bitlocker_manage::BitLockerUnlockMethod::Password,
            Zeroizing::new(String::new()),
        ),
        "resume" => (
            super::native_bitlocker_manage::BitLockerManageAction::ResumeProtection,
            super::native_bitlocker_manage::BitLockerUnlockMethod::Password,
            Zeroizing::new(String::new()),
        ),
        _ => unreachable!(),
    };
    let intent = super::native_bitlocker_manage::build_intent(
        &inventory,
        &volume,
        action,
        unlock_method,
        secret.to_string(),
    )
    .map_err(|error| anyhow!(error))?;
    let message =
        super::native_bitlocker_manage::execute_intent(intent).map_err(|error| anyhow!(error))?;
    Ok(json!({"success":true,"volume":volume,"operation":operation,"message":message}))
}

fn read_secret_stdin(invocation: &ToolInvocation) -> Result<Zeroizing<String>> {
    if !flag(invocation, "secret-stdin") {
        return Err(anyhow!("BitLocker unlock requires --secret-stdin"));
    }
    let stdin = std::io::stdin();
    let mut line = Zeroizing::new(String::new());
    stdin
        .lock()
        .take(16 * 1024)
        .read_line(&mut line)
        .map_err(|error| anyhow!("read BitLocker secret from stdin: {error}"))?;
    while line.ends_with(['\r', '\n']) {
        line.pop();
    }
    if line.is_empty() {
        return Err(anyhow!("BitLocker secret read from stdin is empty"));
    }
    Ok(line)
}

fn password_plan(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ResetPassword)?;
    let request = password_request(invocation)?;
    let accounts = super::native_password_reset::load_password_reset_accounts(&request.target)
        .map_err(|error| anyhow!(error))?;
    if !accounts.iter().any(|account| {
        account
            .username
            .eq_ignore_ascii_case(request.account.trim())
    }) {
        return Err(anyhow!("selected account is not in the fresh inventory"));
    }
    plan_json(
        NativeToolAction::ResetPassword,
        json!({
            "target":password_target_json(&request.target),"account":request.account,
            "password_will_be_cleared":true,"account_will_be_enabled":true,
        }),
    )
}

fn password_run(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ResetPassword)?;
    let request = password_request(invocation)?;
    let result = super::native_password_reset::execute_password_reset(&request)
        .map_err(|error| anyhow!(error))?;
    Ok(json!({
        "target":password_target_json(&result.target),"account":result.account,
        "password_cleared":result.password_cleared,"account_enabled":result.account_enabled,
    }))
}

fn password_request(
    invocation: &ToolInvocation,
) -> Result<super::native_password_reset::PasswordResetRequest> {
    let request = super::native_password_reset::PasswordResetRequest {
        target: password_target(required(invocation, "target")?)?,
        account: required(invocation, "account")?.to_owned(),
    };
    super::native_password_reset::validate_request(&request).map_err(|error| anyhow!(error))?;
    Ok(request)
}

fn expand_c_plan(invocation: &ToolInvocation) -> Result<Value> {
    let analysis = expand_request_analysis(invocation)?;
    plan_json(
        NativeToolAction::ExpandC,
        json!({
            "target":"current_windows_volume","target_size_mb":target_size_mb(invocation)?,
            "analysis":expand_analysis_json(&analysis),"requires_partition_move":false,
        }),
    )
}

fn expand_c_run(invocation: &ToolInvocation) -> Result<Value> {
    ensure_environment(NativeToolAction::ExpandC)?;
    let analysis = expand_request_analysis(invocation)?;
    let target_size_mb = target_size_mb(invocation)?;
    let config = super::app_config::AppConfig::load_strict()?;
    let pe = select_cached_pe(false)?;
    let receiver = super::native_expand_c_executor::start_expand_c_handoff(
        super::native_expand_c_executor::ExpandCHandoffRequest {
            target_partition: lr_core::windows_storage::current_windows_drive_letter()
                .map_err(|error| anyhow!(error))?,
            expected_disk: analysis.disk.clone(),
            expected_partition_number: Some(analysis.partition_number),
            target_size_mb,
            use_maximum: false,
            analyzed_current_size_mb: analysis.current_size_mb,
            analyzed_max_size_mb: analysis.max_size_mb,
            analyzed_no_move_max_mb: analysis.no_move_max_mb,
            strict_analysis_snapshot: true,
            borrow_from_left: false,
            donor_target_size_mb: 0,
            minimum_free_mb: 1024,
            wim_engine: config.wim_engine,
            pe,
        },
    )
    .map_err(|error| anyhow!(error))?;
    loop {
        match receiver
            .recv()
            .map_err(|_| anyhow!("expand-C preparation worker disconnected"))?
        {
            super::native_expand_c_executor::ExpandCWorkerMessage::Progress(detail) => {
                super::cli::emit_progress(
                    json!({"event":"tool_progress","tool":"expand-c","detail":detail}),
                );
            }
            super::native_expand_c_executor::ExpandCWorkerMessage::ReadyToReboot => {
                lr_core::windows_shutdown::schedule_restart(
                    3,
                    "LetRecovery 正在重启到 PE 扩容环境...",
                )
                .map_err(|error| {
                    anyhow!(
                        "expand-C handoff is committed but restart could not be scheduled: {error}"
                    )
                })?;
                return Ok(
                    json!({"prepared":true,"restart_scheduled":true,"target_size_mb":target_size_mb}),
                );
            }
            super::native_expand_c_executor::ExpandCWorkerMessage::Failed(error) => {
                return Err(anyhow!(error));
            }
        }
    }
}

fn expand_request_analysis(
    invocation: &ToolInvocation,
) -> Result<super::native_expand_c_controller::NativeExpandCAnalysis> {
    ensure_environment(NativeToolAction::ExpandC)?;
    let analysis =
        super::native_expand_c_controller::analyze_expand_c().map_err(|error| anyhow!(error))?;
    if !analysis.found || !analysis.can_expand {
        return Err(anyhow!(if analysis.reason.is_empty() {
            "current Windows volume cannot be expanded".to_owned()
        } else {
            analysis.reason.clone()
        }));
    }
    let target = target_size_mb(invocation)?;
    let minimum = analysis
        .current_size_mb
        .max(analysis.used_mb.saturating_add(1024));
    if target < minimum || target > analysis.no_move_max_mb.min(analysis.max_size_mb) {
        return Err(anyhow!(
            "--target-size-mb must be in the current safe range {minimum}..={}",
            analysis.no_move_max_mb.min(analysis.max_size_mb)
        ));
    }
    Ok(analysis)
}

fn target_size_mb(invocation: &ToolInvocation) -> Result<u64> {
    required(invocation, "target-size-mb")?
        .parse::<u64>()
        .map_err(|_| anyhow!("--target-size-mb must be an unsigned integer"))
}

fn pe_maintenance_plan() -> Result<Value> {
    ensure_environment(NativeToolAction::EnterPeMaintenance)?;
    let config = super::app_config::AppConfig::load_strict()?;
    if !config.pe_maintenance_entry_enabled {
        return Err(anyhow!(
            "PE maintenance entry is disabled by pe_maintenance_entry_enabled"
        ));
    }
    let pe = select_cached_pe(true)?;
    plan_json(
        NativeToolAction::EnterPeMaintenance,
        json!({
            "pe_filename":pe.filename,"pe_display_name":pe.display_name,
            "bitlocker_recovery_collection":"best_effort",
            "bitlocker_decryption":false,"letrecovery_window":"hidden",
        }),
    )
}

fn pe_maintenance_run() -> Result<Value> {
    ensure_environment(NativeToolAction::EnterPeMaintenance)?;
    let config = super::app_config::AppConfig::load_strict()?;
    if !config.pe_maintenance_entry_enabled {
        return Err(anyhow!(
            "PE maintenance entry is disabled by pe_maintenance_entry_enabled"
        ));
    }
    let pe = select_cached_pe(true)?;
    super::pe::enter_pe_maintenance(&pe, &config.language)?;
    Ok(json!({"prepared":true,"restart_scheduled":true,"pe_filename":pe.filename}))
}

fn select_cached_pe(official_only: bool) -> Result<crate::download::config::OnlinePE> {
    let mut catalogue = crate::download::config::PeCache::load_strict()?.unwrap_or_default();
    if official_only {
        catalogue.retain(|pe| pe.filename.eq_ignore_ascii_case("LetRecovery_PE.wim"));
    }
    if let Some(pe) = catalogue.into_iter().find(|pe| {
        matches!(
            super::pe::PeManager::check_cached_pe(
                &pe.filename,
                pe.sha256.as_deref(),
                pe.md5.as_deref(),
            ),
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Ready { .. })
        )
    }) {
        return Ok(pe);
    }
    let filename = "LetRecovery_PE.wim";
    if matches!(
        super::pe::PeManager::find_cached_pe(filename, None, None),
        Ok(lr_core::cached_artifact::CachedArtifactPresence::Present { .. })
    ) {
        return Ok(crate::download::config::OnlinePE {
            download_url: String::new(),
            display_name: "LetRecovery PE".into(),
            filename: filename.into(),
            md5: None,
            sha256: None,
        });
    }
    Err(anyhow!("no cached PE is available"))
}

fn hardware_inspect() -> Result<Value> {
    ensure_environment(NativeToolAction::HardwareInspector)?;
    let value = super::hardware_inspector::HardwareInspectorSnapshot::collect()
        .map_err(|error| anyhow!(error))?;
    Ok(json!({
        "computer":{
            "name":value.base.computer_name,"manufacturer":value.base.computer_manufacturer,
            "model":value.base.computer_model,"serial_number":value.base.system_serial_number,
            "device_type":value.base.device_type.to_string(),
        },
        "cpu":{
            "name":value.base.cpu.name,"manufacturer":value.base.cpu.manufacturer,
            "cores":value.base.cpu.cores,"logical_processors":value.base.cpu.logical_processors,
            "max_clock_speed_mhz":value.base.cpu.max_clock_speed,
            "vendor":value.cpuid.vendor,"brand":value.cpuid.brand,"family":value.cpuid.family,
            "model":value.cpuid.model,"stepping":value.cpuid.stepping,
            "features":value.cpuid.features,"microarchitecture":value.cpuid.microarchitecture,
            "process_node":value.cpuid.process_node,"l2_cache_bytes":value.cpuid.l2_cache_bytes,
            "l3_cache_bytes":value.cpuid.l3_cache_bytes,
        },
        "memory":{
            "total_physical_bytes":value.base.memory.total_physical,
            "available_physical_bytes":value.base.memory.available_physical,
            "load_percent":value.base.memory.memory_load,
            "modules":value.smbios.memory_modules.into_iter().map(|module| json!({
                "locator":module.locator,"bank":module.bank,"manufacturer":module.manufacturer,
                "part_number":module.part_number,"serial_number":module.serial_number,
                "memory_type":module.memory_type,"size_bytes":module.size_bytes,
                "speed_mts":module.speed_mts,"configured_speed_mts":module.configured_speed_mts,
            })).collect::<Vec<_>>()
        },
        "firmware":{
            "bios_vendor":value.smbios.bios_vendor,"bios_version":value.smbios.bios_version,
            "bios_date":value.smbios.bios_date,"board_manufacturer":value.smbios.board_manufacturer,
            "board_product":value.smbios.board_product,"board_version":value.smbios.board_version,
        },
        "graphics":value.graphics.into_iter().map(|gpu| json!({
            "name":gpu.name,"vendor_id":gpu.vendor_id,"device_id":gpu.device_id,
            "subsystem_id":gpu.subsystem_id,"revision":gpu.revision,
            "dedicated_video_memory":gpu.dedicated_video_memory,
            "shared_system_memory":gpu.shared_system_memory,"software_adapter":gpu.software_adapter,
            "architecture":gpu.architecture,"process_node":gpu.process_node,
            "core_configuration":gpu.core_configuration,
        })).collect::<Vec<_>>(),
        "storage":value.disks.into_iter().map(|disk| json!({
            "disk_index":disk.disk.disk_index,"model":disk.disk.model,"size_bytes":disk.disk.size,
            "serial_number":disk.disk.serial_number,"firmware_revision":disk.disk.firmware_revision,
            "interface_type":disk.disk.interface_type,"partition_style":disk.disk.partition_style,
            "is_ssd":disk.disk.is_ssd,"trim_enabled":disk.trim_enabled,
            "incurs_seek_penalty":disk.incurs_seek_penalty,
            "nvme_health":disk.nvme_health.map(|health| json!({
                "health_percentage":health.health_percentage,"temperature_celsius":health.temperature_celsius,
                "data_read_bytes":health.data_read_bytes.to_string(),
                "data_written_bytes":health.data_written_bytes.to_string(),
                "power_cycles":health.power_cycles.to_string(),"power_on_hours":health.power_on_hours.to_string(),
                "unsafe_shutdowns":health.unsafe_shutdowns.to_string(),"media_errors":health.media_errors.to_string(),
                "critical_warning":health.critical_warning,
            })),
        })).collect::<Vec<_>>(),
    }))
}

fn canonical_drive(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches(['\\', '/']);
    match value.as_bytes() {
        [letter] if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        _ => Err(anyhow!("invalid drive root {value:?}")),
    }
}

fn inventory_target_value(value: &str) -> Result<String> {
    if value.trim().eq_ignore_ascii_case("current") {
        Ok("__CURRENT__".into())
    } else {
        canonical_drive(value)
    }
}

fn appx_target(value: &str) -> Result<super::native_appx::AppxTarget> {
    if value.trim().eq_ignore_ascii_case("current") {
        Ok(super::native_appx::AppxTarget::CurrentSystem)
    } else {
        Ok(super::native_appx::AppxTarget::OfflineWindows(
            canonical_drive(value)?,
        ))
    }
}

fn appx_target_json(value: &super::native_appx::AppxTarget) -> String {
    match value {
        super::native_appx::AppxTarget::CurrentSystem => "current".into(),
        super::native_appx::AppxTarget::OfflineWindows(root) => root.clone(),
    }
}

fn password_target(value: &str) -> Result<super::native_password_reset::PasswordResetTarget> {
    if value.trim().eq_ignore_ascii_case("current") {
        Ok(super::native_password_reset::PasswordResetTarget::CurrentSystem)
    } else {
        Ok(
            super::native_password_reset::PasswordResetTarget::OfflineWindows(canonical_drive(
                value,
            )?),
        )
    }
}

fn password_target_json(value: &super::native_password_reset::PasswordResetTarget) -> String {
    match value {
        super::native_password_reset::PasswordResetTarget::CurrentSystem => "current".into(),
        super::native_password_reset::PasswordResetTarget::OfflineWindows(root) => root.clone(),
    }
}

fn require_detected_windows_target(target: &str, include_current: bool) -> Result<()> {
    let targets =
        super::native_tool_inventory::load_windows_targets(&current_partitions()?, include_current)
            .map_err(|error| anyhow!(error))?;
    if targets
        .iter()
        .any(|entry| entry.value.eq_ignore_ascii_case(target))
    {
        Ok(())
    } else {
        Err(anyhow!(
            "Windows target is not in the fresh inventory: {target}"
        ))
    }
}

fn read_plain_text(path: &Path, maximum_bytes: u64) -> Result<String> {
    let file = super::cli_config::open_plain_regular_file(path)?;
    let length = file.metadata()?.len();
    if length > maximum_bytes {
        return Err(anyhow!(
            "input file exceeds the {maximum_bytes}-byte limit: {}",
            path.display()
        ));
    }
    let mut content = String::new();
    file.take(maximum_bytes.saturating_add(1))
        .read_to_string(&mut content)?;
    if content.len() as u64 > maximum_bytes {
        return Err(anyhow!(
            "input file exceeds the {maximum_bytes}-byte limit: {}",
            path.display()
        ));
    }
    Ok(content)
}

fn read_plain_lines(path: &Path, maximum_bytes: u64, maximum_lines: usize) -> Result<Vec<String>> {
    let content = read_plain_text(path, maximum_bytes)?;
    let lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if lines.len() > maximum_lines {
        return Err(anyhow!(
            "input file exceeds the {maximum_lines}-line limit: {}",
            path.display()
        ));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn every_native_tool_has_exactly_one_public_name() {
        assert_eq!(TOOL_NAMES.len(), NativeToolAction::ALL.len());
        for action in NativeToolAction::ALL {
            assert_eq!(
                TOOL_NAMES
                    .iter()
                    .filter(|(_, candidate)| *candidate == action)
                    .count(),
                1,
                "{action:?}"
            );
        }
    }

    #[test]
    fn live_mutations_require_yes_but_plans_reject_it() {
        assert!(parse(&args(&[
            "lr",
            "tool",
            "batch-format",
            "run",
            "--drives",
            "D:",
            "--file-system",
            "NTFS"
        ]))
        .is_err());
        assert!(parse(&args(&[
            "lr",
            "tool",
            "batch-format",
            "run",
            "--drives",
            "D:",
            "--file-system",
            "NTFS",
            "--yes"
        ]))
        .unwrap()
        .is_live());
        assert!(parse(&args(&[
            "lr",
            "tool",
            "batch-format",
            "plan",
            "--drives",
            "D:",
            "--file-system",
            "NTFS",
            "--yes"
        ]))
        .is_err());
    }

    #[test]
    fn bitlocker_secrets_are_never_command_line_options() {
        assert!(parse(&args(&[
            "lr",
            "tool",
            "bitlocker",
            "run",
            "--volume",
            "D:",
            "--operation",
            "unlock-recovery",
            "--password",
            "secret",
            "--yes"
        ]))
        .is_err());
        let parsed = parse(&args(&[
            "lr",
            "tool",
            "bitlocker",
            "run",
            "--volume",
            "D:",
            "--operation",
            "unlock-recovery",
            "--secret-stdin",
            "--yes",
        ]))
        .unwrap();
        assert!(flag(&parsed, "secret-stdin"));
    }

    #[test]
    fn canonical_drive_rejects_paths_and_shell_text() {
        assert_eq!(canonical_drive(" d:\\ ").unwrap(), "D:");
        for invalid in ["", "D:\\Windows", "1:", "D: & whoami"] {
            assert!(canonical_drive(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn list_is_exact_and_unknown_tools_fail() {
        assert_eq!(parse(&args(&["lr", "tool", "list"])).unwrap().name, "list");
        assert!(parse(&args(&["lr", "tool", "list", "extra"])).is_err());
        assert!(parse(&args(&["lr", "tool", "unknown", "inspect"])).is_err());
    }

    #[test]
    fn every_public_tool_has_a_parseable_representative_command() {
        let commands: &[&[&str]] = &[
            &["lr", "tool", "nvidia-driver", "inventory"],
            &["lr", "tool", "partition-copy", "inventory"],
            &["lr", "tool", "batch-format", "inventory"],
            &["lr", "tool", "storage-driver", "inventory"],
            &["lr", "tool", "quick-partition", "inventory"],
            &["lr", "tool", "appx", "inventory", "--target", "current"],
            &["lr", "tool", "driver-transfer", "inventory"],
            &["lr", "tool", "repair-boot", "inventory"],
            &["lr", "tool", "network-info", "inspect"],
            &["lr", "tool", "software-list", "inspect"],
            &["lr", "tool", "time-sync", "plan"],
            &["lr", "tool", "ghost", "plan"],
            &[
                "lr",
                "tool",
                "gho-password",
                "read",
                "--path",
                r"C:\image.gho",
            ],
            &["lr", "tool", "reset-network", "plan"],
            &["lr", "tool", "space-sniffer", "plan"],
            &[
                "lr",
                "tool",
                "verify-image",
                "inspect",
                "--path",
                r"C:\image.wim",
            ],
            &["lr", "tool", "bitlocker", "inventory"],
            &[
                "lr",
                "tool",
                "file-hash",
                "inspect",
                "--path",
                r"C:\file.bin",
            ],
            &[
                "lr",
                "tool",
                "reset-password",
                "inventory",
                "--target",
                "current",
            ],
            &["lr", "tool", "expand-c", "analyze"],
            &["lr", "tool", "hardware-inspect", "inspect"],
            &["lr", "tool", "pe-maintenance", "plan"],
        ];
        assert_eq!(commands.len(), TOOL_NAMES.len());
        for (command, (expected_name, _)) in commands.iter().zip(TOOL_NAMES.iter()) {
            let parsed = parse(&args(command)).unwrap_or_else(|error| {
                panic!("representative command {command:?} failed: {error}")
            });
            assert_eq!(&parsed.name, expected_name);
        }
    }

    #[test]
    fn administrator_classification_covers_live_and_sensitive_reads_only() {
        let read_only = parse(&args(&["lr", "tool", "network-info", "inspect"])).unwrap();
        assert!(!read_only.is_live());
        assert!(!read_only.requires_administrator());

        let sensitive = parse(&args(&[
            "lr",
            "tool",
            "bitlocker",
            "read-key",
            "--volume",
            "D:",
        ]))
        .unwrap();
        assert!(!sensitive.is_live());
        assert!(sensitive.requires_administrator());

        let live = parse(&args(&["lr", "tool", "time-sync", "run", "--yes"])).unwrap();
        assert!(live.is_live());
        assert!(live.requires_administrator());
    }
}
