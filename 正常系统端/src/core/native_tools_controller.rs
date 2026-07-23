//! Side-effect-free routing for the native toolbox page.
//!
//! This module preserves all legacy toolbox entry points while keeping the native
//! window message handler away from operational code.  A plan only identifies the existing
//! dialog/action boundary and its safety class; it never starts a process, scans a disk, changes
//! Windows state, or performs a privileged operation.

/// Canonical, presentation-independent intent for every native toolbox entry.
///
/// The Win32 page currently uses the same order for its command IDs; keeping the canonical copy
/// in core prevents routing policy from depending on a private UI module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeToolAction {
    NvidiaDriverRemoval,
    PartitionCopy,
    BatchFormat,
    ImportStorageDriver,
    QuickPartition,
    RemoveAppx,
    DriverBackupRestore,
    RepairBoot,
    NetworkInformation,
    SoftwareList,
    TimeSynchronization,
    RunGhost,
    ReadGhoPassword,
    ResetNetwork,
    RunSpaceSniffer,
    VerifyImage,
    ManageBitLocker,
    VerifyFileHash,
    ResetPassword,
    ExpandC,
    HardwareInspector,
}

impl NativeToolAction {
    pub const FIRST_NATIVE_COMMAND_ID: u16 = 5_100;

    /// Existing command IDs remain stable; new tools are appended.
    pub const ALL: [Self; 21] = [
        Self::NvidiaDriverRemoval,
        Self::PartitionCopy,
        Self::BatchFormat,
        Self::ImportStorageDriver,
        Self::QuickPartition,
        Self::RemoveAppx,
        Self::DriverBackupRestore,
        Self::RepairBoot,
        Self::NetworkInformation,
        Self::SoftwareList,
        Self::TimeSynchronization,
        Self::RunGhost,
        Self::ReadGhoPassword,
        Self::ResetNetwork,
        Self::RunSpaceSniffer,
        Self::VerifyImage,
        Self::ManageBitLocker,
        Self::VerifyFileHash,
        Self::ResetPassword,
        Self::ExpandC,
        Self::HardwareInspector,
    ];

    /// Converts the stable zero-based native button position without accepting unknown IDs.
    pub const fn from_native_index(index: usize) -> Option<Self> {
        if index < Self::ALL.len() {
            Some(Self::ALL[index])
        } else {
            None
        }
    }

    /// Converts the native page's stable command ID without accepting IDs from other pages.
    pub const fn from_native_command_id(command_id: u16) -> Option<Self> {
        match command_id.checked_sub(Self::FIRST_NATIVE_COMMAND_ID) {
            Some(index) => Self::from_native_index(index as usize),
            None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolEnvironment {
    Desktop,
    Pe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolAvailability {
    Available,
    DesktopOnly,
    PeOnly,
}

impl ToolAvailability {
    pub const fn supports(self, environment: ToolEnvironment) -> bool {
        matches!(
            (self, environment),
            (Self::Available, _)
                | (Self::DesktopOnly, ToolEnvironment::Desktop)
                | (Self::PeOnly, ToolEnvironment::Pe)
        )
    }
}

/// Safety category of the operation that may eventually follow a toolbox plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSafetyClass {
    /// Inventory, inspection, or verification without an intended system change.
    ReadOnly,
    /// Read-only access that may reveal a password or other security-sensitive information.
    SensitiveRead,
    /// A privileged Windows setting, package, driver, or clock change.
    SystemMutation,
    /// A boot, partition, volume, or BitLocker operation requiring target revalidation.
    StorageMutation,
    /// Formatting, repartitioning, or copying over a target partition.
    DestructiveStorage,
    /// Account or credential state modification.
    SecurityMutation,
    /// A bundled executable whose subsequent behavior is outside this routing boundary.
    ExternalProgram,
}

impl ToolSafetyClass {
    /// Whether dispatch must remain behind an explicit user action and the existing validation or
    /// confirmation boundary.  Read-only dialogs may preload data; mutating actions may not.
    pub const fn requires_explicit_execution(self) -> bool {
        !matches!(self, Self::ReadOnly | Self::SensitiveRead)
    }
}

/// Existing state/dialog boundary opened by a legacy toolbox button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolDialogRoute {
    NvidiaDriverRemoval,
    PartitionCopy,
    BatchFormat,
    ImportStorageDriver,
    QuickPartition,
    RemoveAppx,
    DriverBackupRestore,
    RepairBoot,
    NetworkInformation,
    SoftwareList,
    TimeSynchronization,
    ReadGhoPassword,
    ResetNetworkConfirmation,
    VerifyImage,
    ManageBitLocker,
    VerifyFileHash,
    ResetPassword,
    ExpandC,
    HardwareInspector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundledToolRoute {
    Ghost64,
    SpaceSniffer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRoute {
    OpenDialog(ToolDialogRoute),
    LaunchBundledTool(BundledToolRoute),
}

/// Data load which the old UI started when opening a dialog.  These are requests only: the
/// planner does not run the corresponding loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolPreload {
    None,
    NvidiaHardwareSummary,
    CopyablePartitions,
    FormatablePartitions,
    QuickPartitionState,
    WindowsPartitions,
    NetworkAdapters,
    SoftwareInventory,
    BitLockerPartitions,
    ExpandCAnalysis,
    HardwareSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeToolPlan {
    pub intent: NativeToolAction,
    pub route: ToolRoute,
    pub preload: ToolPreload,
    pub safety: ToolSafetyClass,
    pub availability: ToolAvailability,
}

impl NativeToolPlan {
    pub const fn is_supported(self, environment: ToolEnvironment) -> bool {
        self.availability.supports(environment)
    }
}

/// Maps a native toolbox click to the established state/action boundary.
///
/// The returned value is deliberately inert.  Callers must enforce `availability`, then open the
/// mapped dialog or perform the explicit bundled-tool launch through the existing action layer.
pub const fn plan_tool(intent: NativeToolAction) -> NativeToolPlan {
    use ToolAvailability::{Available, DesktopOnly, PeOnly};
    use ToolDialogRoute as Dialog;
    use ToolPreload as Preload;
    use ToolRoute::{LaunchBundledTool, OpenDialog};
    use ToolSafetyClass as Safety;

    let (route, preload, safety, availability) = match intent {
        NativeToolAction::NvidiaDriverRemoval => (
            OpenDialog(Dialog::NvidiaDriverRemoval),
            Preload::NvidiaHardwareSummary,
            Safety::SystemMutation,
            Available,
        ),
        NativeToolAction::PartitionCopy => (
            OpenDialog(Dialog::PartitionCopy),
            Preload::CopyablePartitions,
            Safety::DestructiveStorage,
            Available,
        ),
        NativeToolAction::BatchFormat => (
            OpenDialog(Dialog::BatchFormat),
            Preload::FormatablePartitions,
            Safety::DestructiveStorage,
            Available,
        ),
        NativeToolAction::ImportStorageDriver => (
            OpenDialog(Dialog::ImportStorageDriver),
            Preload::None,
            Safety::SystemMutation,
            Available,
        ),
        NativeToolAction::QuickPartition => (
            OpenDialog(Dialog::QuickPartition),
            Preload::QuickPartitionState,
            Safety::DestructiveStorage,
            Available,
        ),
        NativeToolAction::RemoveAppx => (
            OpenDialog(Dialog::RemoveAppx),
            Preload::None,
            Safety::SystemMutation,
            Available,
        ),
        NativeToolAction::DriverBackupRestore => (
            OpenDialog(Dialog::DriverBackupRestore),
            Preload::None,
            Safety::SystemMutation,
            Available,
        ),
        NativeToolAction::RepairBoot => (
            OpenDialog(Dialog::RepairBoot),
            Preload::WindowsPartitions,
            Safety::StorageMutation,
            PeOnly,
        ),
        NativeToolAction::NetworkInformation => (
            OpenDialog(Dialog::NetworkInformation),
            Preload::NetworkAdapters,
            Safety::ReadOnly,
            Available,
        ),
        NativeToolAction::SoftwareList => (
            OpenDialog(Dialog::SoftwareList),
            Preload::SoftwareInventory,
            Safety::ReadOnly,
            DesktopOnly,
        ),
        NativeToolAction::TimeSynchronization => (
            OpenDialog(Dialog::TimeSynchronization),
            Preload::None,
            Safety::SystemMutation,
            Available,
        ),
        NativeToolAction::RunGhost => (
            LaunchBundledTool(BundledToolRoute::Ghost64),
            Preload::None,
            Safety::ExternalProgram,
            Available,
        ),
        NativeToolAction::ReadGhoPassword => (
            OpenDialog(Dialog::ReadGhoPassword),
            Preload::None,
            Safety::SensitiveRead,
            Available,
        ),
        NativeToolAction::ResetNetwork => (
            OpenDialog(Dialog::ResetNetworkConfirmation),
            Preload::None,
            Safety::SystemMutation,
            DesktopOnly,
        ),
        NativeToolAction::RunSpaceSniffer => (
            LaunchBundledTool(BundledToolRoute::SpaceSniffer),
            Preload::None,
            Safety::ExternalProgram,
            Available,
        ),
        NativeToolAction::VerifyImage => (
            OpenDialog(Dialog::VerifyImage),
            Preload::None,
            Safety::ReadOnly,
            Available,
        ),
        NativeToolAction::ManageBitLocker => (
            OpenDialog(Dialog::ManageBitLocker),
            Preload::BitLockerPartitions,
            Safety::StorageMutation,
            Available,
        ),
        NativeToolAction::VerifyFileHash => (
            OpenDialog(Dialog::VerifyFileHash),
            Preload::None,
            Safety::ReadOnly,
            Available,
        ),
        NativeToolAction::ResetPassword => (
            OpenDialog(Dialog::ResetPassword),
            Preload::None,
            Safety::SecurityMutation,
            Available,
        ),
        NativeToolAction::ExpandC => (
            OpenDialog(Dialog::ExpandC),
            Preload::ExpandCAnalysis,
            Safety::DestructiveStorage,
            DesktopOnly,
        ),
        NativeToolAction::HardwareInspector => (
            OpenDialog(Dialog::HardwareInspector),
            Preload::HardwareSnapshot,
            Safety::ReadOnly,
            DesktopOnly,
        ),
    };

    NativeToolPlan {
        intent,
        route,
        preload,
        safety,
        availability,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_native_intents_have_stable_routes() {
        let plans = NativeToolAction::ALL.map(plan_tool);
        assert_eq!(plans.len(), 21);
        for (index, plan) in plans.iter().enumerate() {
            assert_eq!(plan.intent, NativeToolAction::ALL[index]);
            assert_eq!(
                NativeToolAction::from_native_command_id(
                    NativeToolAction::FIRST_NATIVE_COMMAND_ID + index as u16
                ),
                Some(plan.intent)
            );
        }
        assert_eq!(
            NativeToolAction::from_native_command_id(NativeToolAction::FIRST_NATIVE_COMMAND_ID - 1),
            None
        );
        assert_eq!(
            NativeToolAction::from_native_command_id(
                NativeToolAction::FIRST_NATIVE_COMMAND_ID + 21
            ),
            None
        );

        assert_eq!(
            plan_tool(NativeToolAction::RunGhost).route,
            ToolRoute::LaunchBundledTool(BundledToolRoute::Ghost64)
        );
        assert_eq!(
            plan_tool(NativeToolAction::RunSpaceSniffer).route,
            ToolRoute::LaunchBundledTool(BundledToolRoute::SpaceSniffer)
        );
        assert_eq!(
            plans
                .iter()
                .filter(|plan| matches!(plan.route, ToolRoute::OpenDialog(_)))
                .count(),
            19
        );
    }

    #[test]
    fn environment_rules_match_the_legacy_toolbox() {
        for intent in NativeToolAction::ALL {
            let plan = plan_tool(intent);
            match intent {
                NativeToolAction::RepairBoot => {
                    assert!(!plan.is_supported(ToolEnvironment::Desktop));
                    assert!(plan.is_supported(ToolEnvironment::Pe));
                }
                NativeToolAction::SoftwareList
                | NativeToolAction::ResetNetwork
                | NativeToolAction::ExpandC
                | NativeToolAction::HardwareInspector => {
                    assert!(plan.is_supported(ToolEnvironment::Desktop));
                    assert!(!plan.is_supported(ToolEnvironment::Pe));
                }
                _ => {
                    assert!(plan.is_supported(ToolEnvironment::Desktop));
                    assert!(plan.is_supported(ToolEnvironment::Pe));
                }
            }
        }
    }

    #[test]
    fn dangerous_storage_routes_are_never_classified_read_only() {
        for intent in [
            NativeToolAction::PartitionCopy,
            NativeToolAction::BatchFormat,
            NativeToolAction::QuickPartition,
            NativeToolAction::RepairBoot,
            NativeToolAction::ManageBitLocker,
            NativeToolAction::ExpandC,
        ] {
            let safety = plan_tool(intent).safety;
            assert!(matches!(
                safety,
                ToolSafetyClass::DestructiveStorage | ToolSafetyClass::StorageMutation
            ));
            assert!(safety.requires_explicit_execution());
        }
    }

    #[test]
    fn read_only_routes_can_only_request_inventory_preloads() {
        for intent in [
            NativeToolAction::NetworkInformation,
            NativeToolAction::SoftwareList,
            NativeToolAction::VerifyImage,
            NativeToolAction::VerifyFileHash,
            NativeToolAction::HardwareInspector,
        ] {
            assert_eq!(plan_tool(intent).safety, ToolSafetyClass::ReadOnly);
            assert!(!plan_tool(intent).safety.requires_explicit_execution());
        }
        assert_eq!(
            plan_tool(NativeToolAction::ReadGhoPassword).safety,
            ToolSafetyClass::SensitiveRead
        );
    }

    #[test]
    fn legacy_preloads_are_preserved_as_requests_only() {
        assert_eq!(
            plan_tool(NativeToolAction::NvidiaDriverRemoval).preload,
            ToolPreload::NvidiaHardwareSummary
        );
        assert_eq!(
            plan_tool(NativeToolAction::PartitionCopy).preload,
            ToolPreload::CopyablePartitions
        );
        assert_eq!(
            plan_tool(NativeToolAction::BatchFormat).preload,
            ToolPreload::FormatablePartitions
        );
        assert_eq!(
            plan_tool(NativeToolAction::ManageBitLocker).preload,
            ToolPreload::BitLockerPartitions
        );
        assert_eq!(
            plan_tool(NativeToolAction::ExpandC).preload,
            ToolPreload::ExpandCAnalysis
        );
    }
}
