//! Production dispatch for confirmed native toolbox plans.
//! Existing business modules remain the only owners of commands and mutations.

#[cfg(not(feature = "non-elevated-tests"))]
use super::{
    tool_actions as legacy_actions, tool_driver as legacy_driver, tool_network as legacy_network,
    tool_time_sync as legacy_time_sync,
};

use super::native_appx::{RemoveAppxRequest, RemoveAppxResult};
use super::native_batch_format::{BatchFormatExecutionResult, BatchFormatRequest};
use super::native_partition_copy::{PartitionCopyExecutionResult, PartitionCopyRequest};
use super::native_tool_executor::{ConfirmedToolPlan, ExternalToolPlan};
use super::native_tools_controller::{NativeToolAction, ToolSafetyClass};

#[derive(Clone, Debug, PartialEq)]
pub enum NativeToolBackendRequest {
    External(ExternalToolPlan),
    SynchronizeTime(ConfirmedToolPlan),
    ResetNetwork(ConfirmedToolPlan),
    RemoveNvidiaDrivers {
        plan: ConfirmedToolPlan,
        /// `None` means the current online Windows installation.
        offline_target: Option<String>,
    },
    RemoveAppx {
        plan: ConfirmedToolPlan,
        request: RemoveAppxRequest,
    },
    BatchFormat {
        plan: ConfirmedToolPlan,
        request: BatchFormatRequest,
    },
    QuickPartition {
        plan: ConfirmedToolPlan,
        request: super::native_quick_partition::QuickPartitionRequest,
    },
    PartitionCopy {
        plan: ConfirmedToolPlan,
        request: PartitionCopyRequest,
    },
    RepairBoot {
        plan: ConfirmedToolPlan,
        target: String,
        boot_mode: BootRepairMode,
    },
    ImportStorageDriver {
        plan: ConfirmedToolPlan,
        target: String,
        driver_directory: String,
    },
    TransferDrivers {
        plan: ConfirmedToolPlan,
        mode: DriverTransferMode,
        system_partition: Option<String>,
        directory: String,
    },
    ManageBitLocker {
        plan: ConfirmedToolPlan,
        volume: String,
        operation: BitLockerOperation,
    },
    ResetOfflinePassword {
        plan: ConfirmedToolPlan,
        target: String,
        accounts: Vec<String>,
        enable_accounts: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootRepairMode {
    Auto,
    Uefi,
    Legacy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverTransferMode {
    Backup,
    Restore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BitLockerOperation {
    UnlockWithPassword(String),
    UnlockWithRecoveryKey(String),
    SuspendProtection,
    ResumeProtection,
    Decrypt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeToolBackendRoute {
    Ghost,
    SpaceSniffer,
    TimeSynchronization,
    NetworkReset,
    NvidiaOnline,
    NvidiaOffline,
    AppxOnline,
    AppxOffline,
    BatchFormat,
    QuickPartition,
    PartitionCopy,
    RepairBoot,
    ImportStorageDriver,
    BackupDriversOnline,
    BackupDriversOffline,
    RestoreDriversOffline,
    ManageBitLocker,
    ResetOfflinePassword,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeToolBackendResult {
    ExternalStarted,
    TimeSynchronization {
        success: bool,
        message: String,
        old_time: Option<String>,
        new_time: Option<String>,
    },
    NetworkReset {
        succeeded: usize,
        failed: usize,
    },
    NvidiaRemoval {
        success: bool,
        message: String,
        needs_reboot: bool,
        uninstalled_count: usize,
        failed_count: usize,
    },
    AppxRemoval(RemoveAppxResult),
    BatchFormat(BatchFormatExecutionResult),
    PartitionCopy(PartitionCopyExecutionResult),
    Completed {
        message: String,
    },
    BitLocker {
        success: bool,
        message: String,
        error_code: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeToolBackendError {
    DevelopmentBuildDenied,
    PlanMismatch {
        expected: NativeToolAction,
        actual: NativeToolAction,
    },
    InvalidConfirmedPlan,
    InvalidTarget(String),
    Execution(String),
}

impl std::fmt::Display for NativeToolBackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => {
                f.write_str("tool execution is disabled in non-elevated development builds")
            }
            Self::PlanMismatch { expected, actual } => {
                write!(f, "expected {expected:?}, received {actual:?}")
            }
            Self::InvalidConfirmedPlan => f.write_str("invalid confirmed tool plan"),
            Self::InvalidTarget(detail) | Self::Execution(detail) => f.write_str(detail),
        }
    }
}
impl std::error::Error for NativeToolBackendError {}

pub struct NativeToolBackend;

impl NativeToolBackend {
    /// Pure route validation; safe for tests and UI preview.
    pub fn route(
        request: &NativeToolBackendRequest,
    ) -> Result<NativeToolBackendRoute, NativeToolBackendError> {
        match request {
            NativeToolBackendRequest::External(plan) => match plan.action {
                NativeToolAction::RunGhost => Ok(NativeToolBackendRoute::Ghost),
                NativeToolAction::RunSpaceSniffer => Ok(NativeToolBackendRoute::SpaceSniffer),
                actual => Err(NativeToolBackendError::PlanMismatch {
                    expected: NativeToolAction::RunGhost,
                    actual,
                }),
            },
            NativeToolBackendRequest::SynchronizeTime(plan) => {
                validate_confirmed(plan, NativeToolAction::TimeSynchronization)?;
                Ok(NativeToolBackendRoute::TimeSynchronization)
            }
            NativeToolBackendRequest::ResetNetwork(plan) => {
                validate_confirmed(plan, NativeToolAction::ResetNetwork)?;
                Ok(NativeToolBackendRoute::NetworkReset)
            }
            NativeToolBackendRequest::RemoveNvidiaDrivers {
                plan,
                offline_target,
            } => {
                validate_confirmed(plan, NativeToolAction::NvidiaDriverRemoval)?;
                Ok(if offline_target.is_some() {
                    NativeToolBackendRoute::NvidiaOffline
                } else {
                    NativeToolBackendRoute::NvidiaOnline
                })
            }
            NativeToolBackendRequest::RemoveAppx { plan, request } => {
                validate_confirmed(plan, NativeToolAction::RemoveAppx)?;
                super::native_appx::validate_request(request)
                    .map_err(|error| NativeToolBackendError::InvalidTarget(error.to_string()))?;
                Ok(match &request.target {
                    super::native_appx::AppxTarget::CurrentSystem => {
                        NativeToolBackendRoute::AppxOnline
                    }
                    super::native_appx::AppxTarget::OfflineWindows(_) => {
                        NativeToolBackendRoute::AppxOffline
                    }
                })
            }
            NativeToolBackendRequest::BatchFormat { plan, .. } => {
                validate_confirmed(plan, NativeToolAction::BatchFormat)?;
                Ok(NativeToolBackendRoute::BatchFormat)
            }
            NativeToolBackendRequest::QuickPartition { plan, request } => {
                validate_confirmed(plan, NativeToolAction::QuickPartition)?;
                super::native_quick_partition::validate_request(request)
                    .map_err(|error| NativeToolBackendError::InvalidTarget(error.to_string()))?;
                Ok(NativeToolBackendRoute::QuickPartition)
            }
            NativeToolBackendRequest::PartitionCopy { plan, .. } => {
                validate_confirmed(plan, NativeToolAction::PartitionCopy)?;
                Ok(NativeToolBackendRoute::PartitionCopy)
            }
            NativeToolBackendRequest::RepairBoot { plan, .. } => {
                validate_confirmed(plan, NativeToolAction::RepairBoot)?;
                Ok(NativeToolBackendRoute::RepairBoot)
            }
            NativeToolBackendRequest::ImportStorageDriver { plan, .. } => {
                validate_confirmed(plan, NativeToolAction::ImportStorageDriver)?;
                Ok(NativeToolBackendRoute::ImportStorageDriver)
            }
            NativeToolBackendRequest::TransferDrivers {
                plan,
                mode,
                system_partition,
                ..
            } => {
                validate_confirmed(plan, NativeToolAction::DriverBackupRestore)?;
                if *mode == DriverTransferMode::Restore && system_partition.is_none() {
                    return Err(NativeToolBackendError::InvalidTarget(
                        "driver restore requires an offline Windows partition".into(),
                    ));
                }
                Ok(match (mode, system_partition.is_some()) {
                    (DriverTransferMode::Backup, false) => {
                        NativeToolBackendRoute::BackupDriversOnline
                    }
                    (DriverTransferMode::Backup, true) => {
                        NativeToolBackendRoute::BackupDriversOffline
                    }
                    (DriverTransferMode::Restore, _) => {
                        NativeToolBackendRoute::RestoreDriversOffline
                    }
                })
            }
            NativeToolBackendRequest::ManageBitLocker { plan, .. } => {
                validate_confirmed(plan, NativeToolAction::ManageBitLocker)?;
                Ok(NativeToolBackendRoute::ManageBitLocker)
            }
            NativeToolBackendRequest::ResetOfflinePassword {
                plan,
                accounts,
                enable_accounts,
                ..
            } => {
                validate_confirmed(plan, NativeToolAction::ResetPassword)?;
                validate_password_reset_request(accounts, *enable_accounts)?;
                Ok(NativeToolBackendRoute::ResetOfflinePassword)
            }
        }
    }

    pub fn execute(
        request: &NativeToolBackendRequest,
    ) -> Result<NativeToolBackendResult, NativeToolBackendError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = request;
            Err(NativeToolBackendError::DevelopmentBuildDenied)
        }
        #[cfg(not(feature = "non-elevated-tests"))]
        {
            match (Self::route(request)?, request) {
                (NativeToolBackendRoute::Ghost, _) => {
                    legacy_actions::launch_ghost().map_err(NativeToolBackendError::Execution)?;
                    Ok(NativeToolBackendResult::ExternalStarted)
                }
                (NativeToolBackendRoute::SpaceSniffer, _) => {
                    legacy_actions::launch_space_sniffer()
                        .map_err(NativeToolBackendError::Execution)?;
                    Ok(NativeToolBackendResult::ExternalStarted)
                }
                (NativeToolBackendRoute::TimeSynchronization, _) => {
                    let result = legacy_time_sync::sync_time_to_beijing();
                    Ok(NativeToolBackendResult::TimeSynchronization {
                        success: result.success,
                        message: result.message,
                        old_time: result.old_time,
                        new_time: result.new_time,
                    })
                }
                (NativeToolBackendRoute::NetworkReset, _) => {
                    let (succeeded, failed) = legacy_network::reset_network();
                    Ok(NativeToolBackendResult::NetworkReset { succeeded, failed })
                }
                (
                    NativeToolBackendRoute::NvidiaOnline,
                    NativeToolBackendRequest::RemoveNvidiaDrivers { .. },
                ) => Ok(map_nvidia(
                    super::nvidia_driver::uninstall_nvidia_drivers_online()
                        .map_err(|e| NativeToolBackendError::Execution(e.to_string()))?,
                )),
                (
                    NativeToolBackendRoute::NvidiaOffline,
                    NativeToolBackendRequest::RemoveNvidiaDrivers {
                        offline_target: Some(target),
                        ..
                    },
                ) => {
                    validate_offline_target(target)?;
                    Ok(map_nvidia(
                        super::nvidia_driver::uninstall_nvidia_drivers_offline(target)
                            .map_err(|e| NativeToolBackendError::Execution(e.to_string()))?,
                    ))
                }
                (
                    NativeToolBackendRoute::AppxOnline | NativeToolBackendRoute::AppxOffline,
                    NativeToolBackendRequest::RemoveAppx { request, .. },
                ) => super::native_appx::execute(request)
                    .map(NativeToolBackendResult::AppxRemoval)
                    .map_err(|error| NativeToolBackendError::Execution(error.to_string())),
                (
                    NativeToolBackendRoute::BatchFormat,
                    NativeToolBackendRequest::BatchFormat { request, .. },
                ) => {
                    let plan =
                        super::native_batch_format::validate_current(request).map_err(|error| {
                            NativeToolBackendError::InvalidTarget(error.to_string())
                        })?;
                    let result = super::native_batch_format::execute(&plan)
                        .map_err(|error| NativeToolBackendError::Execution(error.to_string()))?;
                    Ok(NativeToolBackendResult::BatchFormat(result))
                }
                (
                    NativeToolBackendRoute::QuickPartition,
                    NativeToolBackendRequest::QuickPartition { request, .. },
                ) => {
                    let result = super::native_quick_partition::execute(request)
                        .map_err(|error| NativeToolBackendError::Execution(error.to_string()))?;
                    if result.success {
                        Ok(completed(&format!(
                            "quick partition completed: {}",
                            result.created_partitions.join(", ")
                        )))
                    } else {
                        Err(NativeToolBackendError::Execution(result.message))
                    }
                }
                (
                    NativeToolBackendRoute::PartitionCopy,
                    NativeToolBackendRequest::PartitionCopy { request, .. },
                ) => {
                    let plan = super::native_partition_copy::validate_current(request).map_err(
                        |error| NativeToolBackendError::InvalidTarget(error.to_string()),
                    )?;
                    let result = super::native_partition_copy::execute(&plan)
                        .map_err(|error| NativeToolBackendError::Execution(error.to_string()))?;
                    Ok(NativeToolBackendResult::PartitionCopy(result))
                }
                (
                    NativeToolBackendRoute::RepairBoot,
                    NativeToolBackendRequest::RepairBoot {
                        target, boot_mode, ..
                    },
                ) => {
                    validate_offline_target(target)?;
                    let use_uefi = resolve_boot_mode(target, *boot_mode)?;
                    super::bcdedit::BootManager::new()
                        .repair_boot_advanced(
                            target,
                            use_uefi,
                            lr_core::boot_pca::BootPcaMode::Auto,
                        )
                        .map_err(|error| NativeToolBackendError::Execution(error.to_string()))?;
                    Ok(completed("boot repair completed"))
                }
                (
                    NativeToolBackendRoute::ImportStorageDriver,
                    NativeToolBackendRequest::ImportStorageDriver {
                        target,
                        driver_directory,
                        ..
                    },
                ) => {
                    validate_offline_target(target)?;
                    validate_directory(driver_directory)?;
                    legacy_driver::import_drivers_offline(target, driver_directory)
                        .map_err(NativeToolBackendError::Execution)?;
                    Ok(completed("storage drivers imported"))
                }
                (
                    NativeToolBackendRoute::BackupDriversOnline,
                    NativeToolBackendRequest::TransferDrivers { directory, .. },
                ) => {
                    validate_nonempty_path(directory)?;
                    legacy_driver::export_drivers_online(directory)
                        .map_err(NativeToolBackendError::Execution)?;
                    Ok(completed("drivers backed up"))
                }
                (
                    NativeToolBackendRoute::BackupDriversOffline,
                    NativeToolBackendRequest::TransferDrivers {
                        system_partition: Some(target),
                        directory,
                        ..
                    },
                ) => {
                    validate_offline_target(target)?;
                    validate_nonempty_path(directory)?;
                    legacy_driver::export_drivers_offline(target, directory)
                        .map_err(NativeToolBackendError::Execution)?;
                    Ok(completed("offline drivers backed up"))
                }
                (
                    NativeToolBackendRoute::RestoreDriversOffline,
                    NativeToolBackendRequest::TransferDrivers {
                        system_partition: Some(target),
                        directory,
                        ..
                    },
                ) => {
                    validate_offline_target(target)?;
                    validate_directory(directory)?;
                    legacy_driver::import_drivers_offline(target, directory)
                        .map_err(NativeToolBackendError::Execution)?;
                    Ok(completed("drivers restored"))
                }
                (
                    NativeToolBackendRoute::ManageBitLocker,
                    NativeToolBackendRequest::ManageBitLocker {
                        volume, operation, ..
                    },
                ) => {
                    validate_offline_target(volume)?;
                    execute_bitlocker(volume, operation)
                }
                (
                    NativeToolBackendRoute::ResetOfflinePassword,
                    NativeToolBackendRequest::ResetOfflinePassword {
                        target,
                        accounts,
                        enable_accounts,
                        ..
                    },
                ) => {
                    validate_offline_target(target)?;
                    validate_password_reset_request(accounts, *enable_accounts)?;

                    // Resolve every account before the first mutation. This avoids partially
                    // changing a batch merely because a later requested account does not exist.
                    let available = lr_core::sam::list_accounts(target)
                        .map_err(|error| NativeToolBackendError::Execution(error.to_string()))?;
                    for requested in accounts {
                        if !available
                            .iter()
                            .any(|account| account.username.eq_ignore_ascii_case(requested.trim()))
                        {
                            return Err(NativeToolBackendError::Execution(format!(
                                "account was not found; SAM was not changed: {:?}",
                                requested.trim()
                            )));
                        }
                    }

                    for account in accounts {
                        let changed = lr_core::sam::clear_account_password(target, account.trim())
                            .map_err(|error| {
                                NativeToolBackendError::Execution(error.to_string())
                            })?;
                        if !changed {
                            return Err(NativeToolBackendError::Execution(format!(
                                "account was not changed: {:?}",
                                account.trim()
                            )));
                        }
                    }
                    Ok(completed("account passwords cleared and accounts enabled"))
                }
                _ => Err(NativeToolBackendError::InvalidConfirmedPlan),
            }
        }
    }
}

fn validate_confirmed(
    plan: &ConfirmedToolPlan,
    expected: NativeToolAction,
) -> Result<(), NativeToolBackendError> {
    if plan.action != expected {
        return Err(NativeToolBackendError::PlanMismatch {
            expected,
            actual: plan.action,
        });
    }
    if !plan.safety.requires_explicit_execution()
        || matches!(
            plan.safety,
            ToolSafetyClass::ReadOnly | ToolSafetyClass::SensitiveRead
        )
    {
        return Err(NativeToolBackendError::InvalidConfirmedPlan);
    }
    Ok(())
}

fn validate_offline_target(target: &str) -> Result<(), NativeToolBackendError> {
    if matches!(target.trim().as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) {
        Ok(())
    } else {
        Err(NativeToolBackendError::InvalidTarget(format!(
            "invalid offline Windows partition: {target:?}"
        )))
    }
}

fn validate_nonempty_path(path: &str) -> Result<(), NativeToolBackendError> {
    if path.trim().is_empty() {
        Err(NativeToolBackendError::InvalidTarget(
            "directory path is empty".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_password_reset_request(
    accounts: &[String],
    enable_accounts: bool,
) -> Result<(), NativeToolBackendError> {
    if accounts.is_empty() {
        return Err(NativeToolBackendError::InvalidTarget(
            "at least one account must be selected".into(),
        ));
    }
    if accounts.iter().any(|account| account.trim().is_empty()) {
        return Err(NativeToolBackendError::InvalidTarget(
            "account name is empty".into(),
        ));
    }
    if !enable_accounts {
        return Err(NativeToolBackendError::InvalidTarget(
            "password-only reset is unavailable because the shared SAM operation also enables the account"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(not(feature = "non-elevated-tests"))]
fn resolve_boot_mode(target: &str, mode: BootRepairMode) -> Result<bool, NativeToolBackendError> {
    match mode {
        BootRepairMode::Uefi => Ok(true),
        BootRepairMode::Legacy => Ok(false),
        BootRepairMode::Auto => {
            let partitions = super::disk::DiskManager::get_partitions()
                .map_err(|error| NativeToolBackendError::Execution(error.to_string()))?;
            let partition = partitions
                .iter()
                .find(|partition| partition.letter.eq_ignore_ascii_case(target))
                .ok_or_else(|| {
                    NativeToolBackendError::InvalidTarget(format!(
                        "cannot resolve partition style for {target:?}"
                    ))
                })?;
            match partition.partition_style {
                super::disk::PartitionStyle::GPT => Ok(true),
                super::disk::PartitionStyle::MBR => Ok(false),
                super::disk::PartitionStyle::Unknown => Err(NativeToolBackendError::InvalidTarget(
                    format!("partition style is unknown for {target:?}"),
                )),
            }
        }
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn validate_directory(path: &str) -> Result<(), NativeToolBackendError> {
    validate_nonempty_path(path)?;
    if std::path::Path::new(path).is_dir() {
        Ok(())
    } else {
        Err(NativeToolBackendError::InvalidTarget(format!(
            "directory does not exist: {path:?}"
        )))
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn completed(message: &str) -> NativeToolBackendResult {
    NativeToolBackendResult::Completed {
        message: message.into(),
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn execute_bitlocker(
    volume: &str,
    operation: &BitLockerOperation,
) -> Result<NativeToolBackendResult, NativeToolBackendError> {
    let manager = super::bitlocker::BitLockerManager::new();
    match operation {
        BitLockerOperation::UnlockWithPassword(secret) => {
            if secret.is_empty() {
                return Err(NativeToolBackendError::InvalidTarget(
                    "BitLocker password is empty".into(),
                ));
            }
            let result = manager.unlock_with_password(volume, secret);
            Ok(NativeToolBackendResult::BitLocker {
                success: result.success,
                message: result.message,
                error_code: result.error_code,
            })
        }
        BitLockerOperation::UnlockWithRecoveryKey(secret) => {
            if secret.is_empty() {
                return Err(NativeToolBackendError::InvalidTarget(
                    "BitLocker recovery key is empty".into(),
                ));
            }
            let result = manager.unlock_with_recovery_key(volume, secret);
            Ok(NativeToolBackendResult::BitLocker {
                success: result.success,
                message: result.message,
                error_code: result.error_code,
            })
        }
        BitLockerOperation::SuspendProtection => manager
            .suspend_protection(volume)
            .map(|message| NativeToolBackendResult::BitLocker {
                success: true,
                message,
                error_code: None,
            })
            .map_err(NativeToolBackendError::Execution),
        BitLockerOperation::ResumeProtection => manager
            .resume_protection(volume)
            .map(|message| NativeToolBackendResult::BitLocker {
                success: true,
                message,
                error_code: None,
            })
            .map_err(NativeToolBackendError::Execution),
        BitLockerOperation::Decrypt => {
            let result = manager.decrypt(volume);
            Ok(NativeToolBackendResult::BitLocker {
                success: result.success,
                message: result.message,
                error_code: result.error_code,
            })
        }
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn map_nvidia(result: super::nvidia_driver::UninstallResult) -> NativeToolBackendResult {
    NativeToolBackendResult::NvidiaRemoval {
        success: result.success,
        message: result.message,
        needs_reboot: result.needs_reboot,
        uninstalled_count: result.uninstalled_count,
        failed_count: result.failed_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::native_tool_executor::{
        plan_execution, ToolExecutionPlan, ToolExecutionRequest,
    };

    fn confirmed(action: NativeToolAction) -> ConfirmedToolPlan {
        match plan_execution(ToolExecutionRequest::NativeAction {
            action,
            confirmed: true,
        }) {
            ToolExecutionPlan::Mutating(plan) => plan,
            other => panic!("expected mutating plan, got {other:?}"),
        }
    }
    fn external(action: NativeToolAction) -> ExternalToolPlan {
        match plan_execution(ToolExecutionRequest::NativeAction {
            action,
            confirmed: true,
        }) {
            ToolExecutionPlan::External(plan) => plan,
            other => panic!("expected external plan, got {other:?}"),
        }
    }

    #[test]
    fn every_implemented_route_is_purely_classified() {
        let cases = [
            (
                NativeToolBackendRequest::External(external(NativeToolAction::RunGhost)),
                NativeToolBackendRoute::Ghost,
            ),
            (
                NativeToolBackendRequest::External(external(NativeToolAction::RunSpaceSniffer)),
                NativeToolBackendRoute::SpaceSniffer,
            ),
            (
                NativeToolBackendRequest::SynchronizeTime(confirmed(
                    NativeToolAction::TimeSynchronization,
                )),
                NativeToolBackendRoute::TimeSynchronization,
            ),
            (
                NativeToolBackendRequest::ResetNetwork(confirmed(NativeToolAction::ResetNetwork)),
                NativeToolBackendRoute::NetworkReset,
            ),
            (
                NativeToolBackendRequest::RemoveNvidiaDrivers {
                    plan: confirmed(NativeToolAction::NvidiaDriverRemoval),
                    offline_target: None,
                },
                NativeToolBackendRoute::NvidiaOnline,
            ),
            (
                NativeToolBackendRequest::RemoveNvidiaDrivers {
                    plan: confirmed(NativeToolAction::NvidiaDriverRemoval),
                    offline_target: Some("D:".into()),
                },
                NativeToolBackendRoute::NvidiaOffline,
            ),
            (
                NativeToolBackendRequest::BatchFormat {
                    plan: confirmed(NativeToolAction::BatchFormat),
                    request: BatchFormatRequest {
                        drives: vec!["D:".into()],
                        file_system: "NTFS".into(),
                        volume_label: "Data".into(),
                    },
                },
                NativeToolBackendRoute::BatchFormat,
            ),
            (
                NativeToolBackendRequest::PartitionCopy {
                    plan: confirmed(NativeToolAction::PartitionCopy),
                    request: PartitionCopyRequest {
                        source: "D:".into(),
                        target: "E:".into(),
                    },
                },
                NativeToolBackendRoute::PartitionCopy,
            ),
            (
                NativeToolBackendRequest::RemoveAppx {
                    plan: confirmed(NativeToolAction::RemoveAppx),
                    request: RemoveAppxRequest {
                        target: crate::core::native_appx::AppxTarget::CurrentSystem,
                        packages: vec!["Contoso.App_1.0_x64__test".into()],
                    },
                },
                NativeToolBackendRoute::AppxOnline,
            ),
            (
                NativeToolBackendRequest::RemoveAppx {
                    plan: confirmed(NativeToolAction::RemoveAppx),
                    request: RemoveAppxRequest {
                        target: crate::core::native_appx::AppxTarget::OfflineWindows("D:".into()),
                        packages: vec!["Contoso.App_1.0_x64__test".into()],
                    },
                },
                NativeToolBackendRoute::AppxOffline,
            ),
        ];
        for (request, expected) in cases {
            assert_eq!(NativeToolBackend::route(&request), Ok(expected));
        }
    }

    #[test]
    fn mismatch_and_unsafe_target_fail_without_execution() {
        let mismatch = NativeToolBackendRequest::ResetNetwork(confirmed(
            NativeToolAction::TimeSynchronization,
        ));
        assert!(matches!(
            NativeToolBackend::route(&mismatch),
            Err(NativeToolBackendError::PlanMismatch { .. })
        ));

        let invalid_offline_appx = NativeToolBackendRequest::RemoveAppx {
            plan: confirmed(NativeToolAction::RemoveAppx),
            request: RemoveAppxRequest {
                target: crate::core::native_appx::AppxTarget::OfflineWindows("D:\\Windows".into()),
                packages: vec!["Contoso.App_1.0_x64__test".into()],
            },
        };
        assert!(matches!(
            NativeToolBackend::route(&invalid_offline_appx),
            Err(NativeToolBackendError::InvalidTarget(_))
        ));
        assert!(validate_offline_target("D:").is_ok());
        assert!(validate_offline_target("D:\\Windows").is_err());
        assert!(validate_offline_target("D: & whoami").is_err());
        assert!(validate_nonempty_path("").is_err());

        let mismatch = NativeToolBackendRequest::BatchFormat {
            plan: confirmed(NativeToolAction::TimeSynchronization),
            request: BatchFormatRequest {
                drives: vec!["D:".into()],
                file_system: "NTFS".into(),
                volume_label: "Data".into(),
            },
        };
        assert!(matches!(
            NativeToolBackend::route(&mismatch),
            Err(NativeToolBackendError::PlanMismatch { .. })
        ));
    }

    #[test]
    fn second_batch_routes_are_typed_and_side_effect_free() {
        let cases = [
            (
                NativeToolBackendRequest::RepairBoot {
                    plan: confirmed(NativeToolAction::RepairBoot),
                    target: "D:".into(),
                    boot_mode: BootRepairMode::Auto,
                },
                NativeToolBackendRoute::RepairBoot,
            ),
            (
                NativeToolBackendRequest::ImportStorageDriver {
                    plan: confirmed(NativeToolAction::ImportStorageDriver),
                    target: "D:".into(),
                    driver_directory: "drivers".into(),
                },
                NativeToolBackendRoute::ImportStorageDriver,
            ),
            (
                NativeToolBackendRequest::TransferDrivers {
                    plan: confirmed(NativeToolAction::DriverBackupRestore),
                    mode: DriverTransferMode::Backup,
                    system_partition: None,
                    directory: "drivers".into(),
                },
                NativeToolBackendRoute::BackupDriversOnline,
            ),
            (
                NativeToolBackendRequest::TransferDrivers {
                    plan: confirmed(NativeToolAction::DriverBackupRestore),
                    mode: DriverTransferMode::Backup,
                    system_partition: Some("D:".into()),
                    directory: "drivers".into(),
                },
                NativeToolBackendRoute::BackupDriversOffline,
            ),
            (
                NativeToolBackendRequest::TransferDrivers {
                    plan: confirmed(NativeToolAction::DriverBackupRestore),
                    mode: DriverTransferMode::Restore,
                    system_partition: Some("D:".into()),
                    directory: "drivers".into(),
                },
                NativeToolBackendRoute::RestoreDriversOffline,
            ),
            (
                NativeToolBackendRequest::ManageBitLocker {
                    plan: confirmed(NativeToolAction::ManageBitLocker),
                    volume: "D:".into(),
                    operation: BitLockerOperation::SuspendProtection,
                },
                NativeToolBackendRoute::ManageBitLocker,
            ),
            (
                NativeToolBackendRequest::ResetOfflinePassword {
                    plan: confirmed(NativeToolAction::ResetPassword),
                    target: "D:".into(),
                    accounts: vec!["Administrator".into()],
                    enable_accounts: true,
                },
                NativeToolBackendRoute::ResetOfflinePassword,
            ),
        ];
        for (request, expected) in cases {
            assert_eq!(NativeToolBackend::route(&request), Ok(expected));
        }

        let missing_target = NativeToolBackendRequest::TransferDrivers {
            plan: confirmed(NativeToolAction::DriverBackupRestore),
            mode: DriverTransferMode::Restore,
            system_partition: None,
            directory: "drivers".into(),
        };
        assert!(matches!(
            NativeToolBackend::route(&missing_target),
            Err(NativeToolBackendError::InvalidTarget(_))
        ));

        for boot_mode in [
            BootRepairMode::Auto,
            BootRepairMode::Uefi,
            BootRepairMode::Legacy,
        ] {
            let request = NativeToolBackendRequest::RepairBoot {
                plan: confirmed(NativeToolAction::RepairBoot),
                target: "D:".into(),
                boot_mode,
            };
            assert_eq!(
                NativeToolBackend::route(&request),
                Ok(NativeToolBackendRoute::RepairBoot)
            );
        }

        for (accounts, enable_accounts) in [(Vec::new(), true), (vec!["Admin".into()], false)] {
            let request = NativeToolBackendRequest::ResetOfflinePassword {
                plan: confirmed(NativeToolAction::ResetPassword),
                target: "D:".into(),
                accounts,
                enable_accounts,
            };
            assert!(matches!(
                NativeToolBackend::route(&request),
                Err(NativeToolBackendError::InvalidTarget(_))
            ));
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_refuses_before_io() {
        let request = NativeToolBackendRequest::External(external(NativeToolAction::RunGhost));
        assert_eq!(
            NativeToolBackend::execute(&request),
            Err(NativeToolBackendError::DevelopmentBuildDenied)
        );

        // Even an invalid drive must be rejected by the development guard
        // before current-volume enumeration or format validation begins.
        let format = NativeToolBackendRequest::BatchFormat {
            plan: confirmed(NativeToolAction::BatchFormat),
            request: BatchFormatRequest {
                drives: vec!["C:".into()],
                file_system: "NTFS".into(),
                volume_label: "Data".into(),
            },
        };
        assert_eq!(
            NativeToolBackend::execute(&format),
            Err(NativeToolBackendError::DevelopmentBuildDenied)
        );
        let copy = NativeToolBackendRequest::PartitionCopy {
            plan: confirmed(NativeToolAction::PartitionCopy),
            request: PartitionCopyRequest {
                source: "C:".into(),
                target: "C:".into(),
            },
        };
        assert_eq!(
            NativeToolBackend::execute(&copy),
            Err(NativeToolBackendError::DevelopmentBuildDenied)
        );
    }
}
