//! Execution boundary for native system-install intents.
//!
//! This module deliberately keeps Win32 controls away from disk mutation.  It
//! describes the legacy direct and desktop-to-PE workflows as ordered phases
//! and delegates each phase to a backend.  The existing implementation in
//! `ui/install_progress.rs` remains the behavioural reference until its
//! individual operations are moved behind [`InstallExecutionBackend`].

use super::native_install_controller::{InstallMode, StartInstallIntent};

/// Stable partition identity captured immediately before an installation.
///
/// A drive letter is not sufficient because DiskPart and WinPE can reassign
/// letters, and disk/partition numbers can be reused after hot-plug or
/// recreation. Direct installs therefore require the exact physical geometry
/// before the backend is allowed to mutate the target.
#[derive(Clone, Copy, Debug)]
pub struct StableTargetIdentity {
    pub disk_number: u32,
    pub partition_number: u32,
    pub disk_size_bytes: u64,
    pub partition_offset_bytes: u64,
    pub partition_size_bytes: u64,
    pub stable_volume: lr_core::windows_storage::StableVolumeIdentity,
}

/// Compares the immutable physical range used to authorize a later partition write.
///
/// Drive letters, partition numbers, containing-disk capacity and UI capacity snapshots are
/// deliberately excluded. Missing disk/range fields fail closed.
pub fn physical_partition_ranges_match(
    expected_disk_number: Option<u32>,
    expected_offset_bytes: Option<u64>,
    expected_extent_bytes: Option<u64>,
    current_disk_number: Option<u32>,
    current_offset_bytes: Option<u64>,
    current_extent_bytes: Option<u64>,
) -> bool {
    matches!(
        (
            expected_disk_number,
            expected_offset_bytes,
            expected_extent_bytes,
            current_disk_number,
            current_offset_bytes,
            current_extent_bytes,
        ),
        (
            Some(expected_disk),
            Some(expected_offset),
            Some(expected_extent),
            Some(current_disk),
            Some(current_offset),
            Some(current_extent),
        ) if expected_disk == current_disk
            && expected_offset == current_offset
            && expected_extent == current_extent
    )
}

impl StableTargetIdentity {
    pub fn matches_stable_volume(
        self,
        actual: lr_core::windows_storage::StableVolumeIdentity,
    ) -> bool {
        lr_core::windows_storage::same_stable_volume_identity(self.stable_volume, actual)
    }

    /// Match the physical partition range, not mutable inventory presentation fields.
    ///
    /// Partition numbers may be renumbered by unrelated layout changes and the containing disk
    /// size is not part of the addressed volume. They remain in the snapshot for diagnostics only.
    pub fn matches_components(
        self,
        disk_number: Option<u32>,
        _partition_number: Option<u32>,
        _disk_size_bytes: Option<u64>,
        partition_offset_bytes: Option<u64>,
        partition_size_bytes: Option<u64>,
    ) -> bool {
        physical_partition_ranges_match(
            Some(self.disk_number),
            Some(self.partition_offset_bytes),
            Some(self.partition_size_bytes),
            disk_number,
            partition_offset_bytes,
            partition_size_bytes,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BitLockerRequirement {
    /// The relevant target/boot volumes are already safe to use.
    #[default]
    Ready,
    /// UI must run the existing unlock dialog before starting an executor.
    UnlockRequired,
    /// Existing decryption has started and must reach NotEncrypted first.
    AwaitDecryption,
}

/// Runtime facts produced by read-only preflight in the native UI/controller.
#[derive(Clone, Copy, Debug, Default)]
pub struct InstallExecutionContext {
    pub stable_target: Option<StableTargetIdentity>,
    pub bitlocker: BitLockerRequirement,
}

/// Ordered operations from the old `install_progress.rs` implementation.
///
/// Variants are intentionally semantic.  The backend is responsible for the
/// existing GHO/WIM/XP, BIOS/UEFI and driver sub-branches using fields already
/// present in `StartInstallIntent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallExecutionPhase {
    InspectBitLocker,
    AwaitBitLockerDecryption,
    VerifyPcaBeforeDiskWrite,

    // Direct workflow.
    ResolveStableTarget,
    RunDiskpartScripts,
    ResolveTargetAfterDiskpart,
    PreparePreinstalledSoftware,
    FormatTarget,
    ExportHostDrivers,
    ApplyXpTextModeSource,
    ApplyGhostImage,
    ApplyWimImage,
    ProcessDrivers,
    RepairBoot,
    StageDirectPreinstalledSoftware,
    ApplyAdvancedOptions,
    FinishDirectInstall,

    // Desktop-to-PE workflow.
    VerifyPeEnvironment,
    InstallPeBootEntry,
    SelectDataPartition,
    PersistPcaCompatibilityPackage,
    ExportDriversToPeData,
    VerifySourceImage,
    CopySourceImage,
    StagePreinstalledSoftware,
    StageUefiSeven,
    StageUserDrivers,
    WritePeInstallConfig,
    ReadyToRebootIntoPe,
}

impl InstallExecutionPhase {
    pub const fn is_via_pe_commit_phase(self) -> bool {
        matches!(
            self,
            Self::WritePeInstallConfig | Self::InstallPeBootEntry | Self::ReadyToRebootIntoPe
        )
    }

    pub const fn mutates_machine(self) -> bool {
        !matches!(
            self,
            Self::InspectBitLocker
                | Self::AwaitBitLockerDecryption
                | Self::VerifyPcaBeforeDiskWrite
                | Self::ResolveStableTarget
                | Self::VerifyPeEnvironment
                | Self::VerifySourceImage
                | Self::PreparePreinstalledSoftware
        )
    }

    /// Relative duration used only to divide one concrete execution plan into overall-progress
    /// ranges. The ranges are accumulated in plan order; no phase owns a global fixed percentage.
    const fn estimated_weight(self, intent: &StartInstallIntent) -> u16 {
        match self {
            Self::InspectBitLocker => 1,
            Self::AwaitBitLockerDecryption | Self::VerifyPcaBeforeDiskWrite => 2,
            Self::ResolveStableTarget | Self::ResolveTargetAfterDiskpart => 1,
            Self::RunDiskpartScripts => 2,
            Self::PreparePreinstalledSoftware => 10,
            Self::FormatTarget => 3,
            Self::ExportHostDrivers | Self::ExportDriversToPeData => 6,
            Self::ApplyXpTextModeSource
            | Self::ApplyGhostImage
            | Self::ApplyWimImage
            | Self::CopySourceImage => 42,
            Self::ProcessDrivers | Self::RepairBoot => 6,
            Self::StageDirectPreinstalledSoftware => {
                if intent.running_in_pe {
                    12
                } else {
                    2
                }
            }
            Self::ApplyAdvancedOptions => 3,
            Self::FinishDirectInstall
            | Self::SelectDataPartition
            | Self::StageUefiSeven
            | Self::ReadyToRebootIntoPe => 1,
            Self::VerifyPeEnvironment
            | Self::StagePreinstalledSoftware
            | Self::StageUserDrivers => 2,
            Self::InstallPeBootEntry | Self::PersistPcaCompatibilityPackage => 3,
            Self::VerifySourceImage => 10,
            Self::WritePeInstallConfig => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallProgressRange {
    pub start: u8,
    pub end: u8,
}

impl InstallProgressRange {
    pub fn map(self, phase_progress: u8) -> u8 {
        let progress = phase_progress.min(100);
        let span = u16::from(self.end.saturating_sub(self.start));
        (u16::from(self.start) + span * u16::from(progress) / 100).min(100) as u8
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallExecutionEvent {
    Started {
        total_phases: usize,
    },
    PhaseStarted {
        index: usize,
        total: usize,
        phase: InstallExecutionPhase,
        cancellable: bool,
        overall: InstallProgressRange,
    },
    Progress {
        phase: InstallExecutionPhase,
        percentage: u8,
        detail: String,
    },
    PhaseCompleted {
        index: usize,
        total: usize,
        phase: InstallExecutionPhase,
        overall_end: u8,
    },
    Completed(InstallExecutionOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallExecutionOutcome {
    DirectInstallCompleted,
    ReadyToRebootIntoPe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallBackendError {
    pub code: &'static str,
    pub detail: String,
}

impl InstallBackendError {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallExecutionError {
    DevelopmentBuildDenied,
    MissingStableTarget,
    BitLockerUnlockRequired,
    XpTextModeRequiresBootRepair,
    Cancelled,
    Backend {
        phase: InstallExecutionPhase,
        source: InstallBackendError,
    },
}

impl InstallExecutionError {
    /// Returns a concise localized message suitable for the progress page.
    ///
    /// Backend `detail` is intentionally excluded here: it remains available through
    /// [`std::fmt::Display`] for logs and support diagnostics. Source-image verification uses a
    /// fixed, non-sensitive support code plus common causes so a screenshot remains actionable.
    pub fn user_message(&self) -> String {
        match self {
            Self::DevelopmentBuildDenied => {
                crate::tr!("开发测试构建禁止执行真实系统安装。")
            }
            Self::MissingStableTarget => {
                crate::tr!("目标分区状态已变化，安装已在写入前停止。请刷新分区后重试。")
            }
            Self::BitLockerUnlockRequired => {
                crate::tr!("目标分区已被 BitLocker 锁定。请先解锁分区，再重新开始安装。")
            }
            Self::XpTextModeRequiresBootRepair => crate::tr!(
                "XP/2003 文本模式安装必须同时写入 NT5 引导。请启用“添加引导”后重试。"
            ),
            Self::Cancelled => crate::tr!("安装已取消。"),
            Self::Backend {
                phase: InstallExecutionPhase::VerifySourceImage,
                ..
            } => crate::tr!(
                "系统镜像校验失败，未复制到 PE 环境。\r\n可能原因：镜像损坏或下载不完整、SWM 分卷缺失、源磁盘读取异常，或安全软件拦截校验组件（诊断代码：IMG_VERIFY_FAILED）。"
            ),
            Self::Backend {
                phase: InstallExecutionPhase::VerifyPcaBeforeDiskWrite,
                source,
            } if matches!(
                source.code,
                "storage_driver_hardware_enumeration"
                    | "storage_driver_selection"
                    | "storage_driver_package_verification"
            ) => crate::tr!(
                "启动存储控制器驱动检查失败，尚未继续写入目标系统。请通过错误弹窗打开日志文件。"
            ),
            Self::Backend {
                phase: InstallExecutionPhase::SelectDataPartition,
                source,
            } if source.code == "no_data_partition" => crate::tr!(
                "没有任何单个分区能容纳全部安装文件。多个分区末尾的空闲区彼此不连续，不能合并成一个普通数据分区；程序不会把磁盘转换为动态跨区卷。请释放一个分区的空间，或连接容量足够的外置磁盘。"
            ),
            Self::Backend { phase, .. } => phase.user_failure_message(),
        }
    }
}

impl InstallExecutionPhase {
    fn user_failure_message(self) -> String {
        match self {
            Self::InspectBitLocker | Self::AwaitBitLockerDecryption => {
                crate::tr!("无法确认目标分区的 BitLocker 状态，安装已安全停止。")
            }
            Self::VerifyPcaBeforeDiskWrite | Self::PersistPcaCompatibilityPackage => {
                crate::tr!("启动签名兼容性检查失败，尚未继续写入目标系统。")
            }
            Self::ResolveStableTarget
            | Self::RunDiskpartScripts
            | Self::ResolveTargetAfterDiskpart
            | Self::FormatTarget => {
                crate::tr!("准备目标磁盘或分区失败，安装已停止。")
            }
            Self::ExportHostDrivers
            | Self::ProcessDrivers
            | Self::ExportDriversToPeData
            | Self::StageUserDrivers => crate::tr!("处理系统驱动失败，安装已停止。"),
            Self::ApplyXpTextModeSource | Self::ApplyGhostImage | Self::ApplyWimImage => {
                crate::tr!("释放系统镜像失败，安装已停止。")
            }
            Self::RepairBoot => crate::tr!("写入 Windows 启动文件失败，安装已停止。"),
            Self::StageDirectPreinstalledSoftware => {
                crate::tr!("准备预装软件到新系统失败，安装已停止。")
            }
            Self::ApplyAdvancedOptions => {
                crate::tr!("应用安装高级选项失败，安装已停止。")
            }
            Self::PreparePreinstalledSoftware => {
                crate::tr!("下载预装软件失败，尚未开始修改目标系统。")
            }
            Self::FinishDirectInstall => crate::tr!("完成系统安装时发生错误。"),
            Self::VerifyPeEnvironment | Self::InstallPeBootEntry | Self::SelectDataPartition => {
                crate::tr!("准备 PE 安装环境失败，未进入重启阶段。")
            }
            Self::VerifySourceImage => crate::tr!("系统镜像校验失败，未复制到 PE 环境。"),
            Self::CopySourceImage => crate::tr!("复制系统镜像到 PE 数据分区失败。"),
            Self::StagePreinstalledSoftware => crate::tr!("复制预装软件到 PE 数据分区失败。"),
            Self::StageUefiSeven => crate::tr!("准备 UEFISeven 启动文件失败。"),
            Self::WritePeInstallConfig => crate::tr!("写入 PE 安装配置失败，未进入重启阶段。"),
            Self::ReadyToRebootIntoPe => crate::tr!("完成 PE 安装交接时发生错误。"),
        }
    }
}

impl std::fmt::Display for InstallExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => formatter
                .write_str("installation execution is disabled in non-elevated development builds"),
            Self::MissingStableTarget => {
                formatter.write_str("a stable disk and partition identity is required")
            }
            Self::BitLockerUnlockRequired => {
                formatter.write_str("BitLocker volumes must be unlocked before installation")
            }
            Self::XpTextModeRequiresBootRepair => {
                formatter.write_str("XP/2003 text-mode installation requires NT5 boot preparation")
            }
            Self::Cancelled => formatter.write_str("installation was cancelled"),
            Self::Backend { phase, source } => write!(
                formatter,
                "installation phase {phase:?} failed ({}): {}",
                source.code, source.detail
            ),
        }
    }
}

impl std::error::Error for InstallExecutionError {}

pub trait InstallExecutionReporter {
    fn report(&mut self, event: InstallExecutionEvent);
}

impl<F> InstallExecutionReporter for F
where
    F: FnMut(InstallExecutionEvent),
{
    fn report(&mut self, event: InstallExecutionEvent) {
        self(event);
    }
}

struct PhaseProgressReporter<'a> {
    downstream: &'a mut dyn InstallExecutionReporter,
    phase: InstallExecutionPhase,
    last_percentage: u8,
}

impl InstallExecutionReporter for PhaseProgressReporter<'_> {
    fn report(&mut self, event: InstallExecutionEvent) {
        match event {
            InstallExecutionEvent::Progress {
                percentage, detail, ..
            } => {
                let percentage = percentage.min(100).max(self.last_percentage);
                self.last_percentage = percentage;
                self.downstream.report(InstallExecutionEvent::Progress {
                    phase: self.phase,
                    percentage,
                    detail,
                });
            }
            other => self.downstream.report(other),
        }
    }
}

pub trait InstallCancellation {
    fn is_cancelled(&self) -> bool;
}

impl<F> InstallCancellation for F
where
    F: Fn() -> bool,
{
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// Side-effect implementation supplied by the migrated install workflow.
///
/// Long-running phases must periodically inspect `cancellation` and may emit
/// fine-grained progress through `reporter`.  A backend error must represent a
/// verified failure; it must never report success after a failed write.
pub trait InstallExecutionBackend {
    fn execute_phase(
        &mut self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        phase: InstallExecutionPhase,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError>;
}

pub struct NativeInstallExecutor;

#[derive(Default)]
struct ViaPeCommitState {
    started: bool,
}

impl ViaPeCommitState {
    fn should_honor_cancellation(&self, cancellation_requested: bool) -> bool {
        cancellation_requested && !self.started
    }

    fn enter_phase(&mut self, mode: InstallMode, phase: InstallExecutionPhase) {
        if mode == InstallMode::ViaPe && phase == InstallExecutionPhase::WritePeInstallConfig {
            self.started = true;
        }
    }

    fn cancellable(&self) -> bool {
        !self.started
    }
}

impl NativeInstallExecutor {
    /// Builds the exact high-level branch without performing any I/O.
    pub fn build_plan(
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
    ) -> Result<Vec<InstallExecutionPhase>, InstallExecutionError> {
        if context.bitlocker == BitLockerRequirement::UnlockRequired {
            return Err(InstallExecutionError::BitLockerUnlockRequired);
        }
        if intent.mode == InstallMode::Direct && context.stable_target.is_none() {
            return Err(InstallExecutionError::MissingStableTarget);
        }
        if intent.options.is_xp_i386 && !intent.options.repair_boot {
            return Err(InstallExecutionError::XpTextModeRequiresBootRepair);
        }

        let mut phases = vec![InstallExecutionPhase::InspectBitLocker];
        if context.bitlocker == BitLockerRequirement::AwaitDecryption {
            phases.push(InstallExecutionPhase::AwaitBitLockerDecryption);
        }
        if intent.options.repair_boot {
            phases.push(InstallExecutionPhase::VerifyPcaBeforeDiskWrite);
        }

        match intent.mode {
            InstallMode::Direct => Self::append_direct_phases(intent, &mut phases),
            InstallMode::ViaPe => Self::append_via_pe_phases(intent, &mut phases),
        }
        Ok(phases)
    }

    /// Allocate monotonic overall-progress ranges from the exact optional phases that will run.
    /// This avoids the old fixed global bands, which moved backwards whenever two optional phases
    /// shared a band or a PE-hosted Direct download occurred after image and boot work.
    pub fn progress_ranges(
        intent: &StartInstallIntent,
        plan: &[InstallExecutionPhase],
    ) -> Vec<InstallProgressRange> {
        let total_weight = plan
            .iter()
            .map(|phase| u32::from(phase.estimated_weight(intent)))
            .sum::<u32>()
            .max(1);
        let mut completed_weight = 0_u32;
        plan.iter()
            .enumerate()
            .map(|(index, phase)| {
                let start = (completed_weight.saturating_mul(100) / total_weight).min(100) as u8;
                completed_weight =
                    completed_weight.saturating_add(u32::from(phase.estimated_weight(intent)));
                let end = if index + 1 == plan.len() {
                    100
                } else {
                    (completed_weight.saturating_mul(100) / total_weight).min(100) as u8
                };
                InstallProgressRange { start, end }
            })
            .collect()
    }

    fn append_direct_phases(intent: &StartInstallIntent, phases: &mut Vec<InstallExecutionPhase>) {
        phases.push(InstallExecutionPhase::ResolveStableTarget);
        if intent.options.export_drivers && !intent.options.is_xp_i386 {
            // Driver preservation must complete while the source Windows volume is still intact.
            phases.push(InstallExecutionPhase::ExportHostDrivers);
        }
        if intent.options.run_diskpart_scripts {
            phases.push(InstallExecutionPhase::RunDiskpartScripts);
            phases.push(InstallExecutionPhase::ResolveTargetAfterDiskpart);
        }
        phases.push(InstallExecutionPhase::VerifySourceImage);
        let has_preinstalled_software = !intent
            .options
            .advanced_options
            .preinstalled_software
            .is_empty();
        if has_preinstalled_software && !intent.running_in_pe {
            phases.push(InstallExecutionPhase::PreparePreinstalledSoftware);
        }
        phases.push(InstallExecutionPhase::FormatTarget);

        if intent.options.is_xp_i386 {
            // XP text-mode setup owns image copying, AHCI/NVMe/USB3 integration
            // and NT5 boot preparation.  The later generic phases must not run.
            phases.push(InstallExecutionPhase::ApplyXpTextModeSource);
            phases.push(InstallExecutionPhase::FinishDirectInstall);
            return;
        }

        phases.push(if intent.is_gho {
            InstallExecutionPhase::ApplyGhostImage
        } else {
            InstallExecutionPhase::ApplyWimImage
        });
        phases.push(InstallExecutionPhase::ProcessDrivers);
        if intent.options.repair_boot {
            phases.push(InstallExecutionPhase::RepairBoot);
        }
        if has_preinstalled_software {
            // On desktop Windows this copies the already downloaded installers. When this normal
            // endpoint runs inside WinPE, this is the real network download into the applied
            // target and therefore receives a larger plan-aware progress weight.
            phases.push(InstallExecutionPhase::StageDirectPreinstalledSoftware);
        }
        phases.push(InstallExecutionPhase::ApplyAdvancedOptions);
        phases.push(InstallExecutionPhase::FinishDirectInstall);
    }

    fn append_via_pe_phases(intent: &StartInstallIntent, phases: &mut Vec<InstallExecutionPhase>) {
        phases.extend([InstallExecutionPhase::VerifyPeEnvironment]);
        if !intent
            .options
            .advanced_options
            .preinstalled_software
            .is_empty()
        {
            phases.push(InstallExecutionPhase::PreparePreinstalledSoftware);
        }
        phases.push(InstallExecutionPhase::SelectDataPartition);
        if intent.options.repair_boot {
            phases.push(InstallExecutionPhase::PersistPcaCompatibilityPackage);
        }
        if intent.options.export_drivers {
            phases.push(InstallExecutionPhase::ExportDriversToPeData);
        }
        phases.extend([
            InstallExecutionPhase::VerifySourceImage,
            InstallExecutionPhase::CopySourceImage,
        ]);
        if !intent
            .options
            .advanced_options
            .preinstalled_software
            .is_empty()
        {
            phases.push(InstallExecutionPhase::StagePreinstalledSoftware);
        }
        if intent.options.repair_boot && intent.options.advanced_options.win7_uefi_patch {
            phases.push(InstallExecutionPhase::StageUefiSeven);
        }
        phases.extend([
            InstallExecutionPhase::StageUserDrivers,
            InstallExecutionPhase::WritePeInstallConfig,
            // Installing the one-shot PE boot entry is the final machine
            // mutation. Every source, package and configuration artifact is
            // fully staged and verified before boot state is touched.
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::ReadyToRebootIntoPe,
        ]);
    }

    #[cfg(any(test, not(feature = "non-elevated-tests")))]
    fn execute_plan(
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        backend: &mut dyn InstallExecutionBackend,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<InstallExecutionOutcome, InstallExecutionError> {
        let plan = Self::build_plan(intent, context)?;
        let progress_ranges = Self::progress_ranges(intent, &plan);
        reporter.report(InstallExecutionEvent::Started {
            total_phases: plan.len(),
        });
        let mut via_pe_commit = ViaPeCommitState::default();
        for (offset, phase) in plan.iter().copied().enumerate() {
            if via_pe_commit.should_honor_cancellation(cancellation.is_cancelled()) {
                return Err(InstallExecutionError::Cancelled);
            }
            via_pe_commit.enter_phase(intent.mode, phase);
            let index = offset + 1;
            let overall = progress_ranges[offset];
            reporter.report(InstallExecutionEvent::PhaseStarted {
                index,
                total: plan.len(),
                phase,
                cancellable: via_pe_commit.cancellable(),
                overall,
            });
            let result = {
                let mut phase_reporter = PhaseProgressReporter {
                    downstream: reporter,
                    phase,
                    last_percentage: 0,
                };
                backend.execute_phase(intent, context, phase, &mut phase_reporter, cancellation)
            };
            if let Err(source) = result {
                if source.code == "cancelled" {
                    return Err(InstallExecutionError::Cancelled);
                }
                return Err(InstallExecutionError::Backend { phase, source });
            }
            reporter.report(InstallExecutionEvent::PhaseCompleted {
                index,
                total: plan.len(),
                phase,
                overall_end: overall.end,
            });
        }
        let outcome = match intent.mode {
            InstallMode::Direct => InstallExecutionOutcome::DirectInstallCompleted,
            InstallMode::ViaPe => InstallExecutionOutcome::ReadyToRebootIntoPe,
        };
        reporter.report(InstallExecutionEvent::Completed(outcome));
        Ok(outcome)
    }

    /// Runs an already validated intent through the production or injected backend.
    ///
    /// Development builds keep the public execution boundary disabled. Unit tests exercise the
    /// same private state-machine loop with inert backends so every phase can be fault-injected
    /// without invoking a Windows API or mutating a disk.
    pub fn execute(
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        backend: &mut dyn InstallExecutionBackend,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<InstallExecutionOutcome, InstallExecutionError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = (intent, context, backend, reporter, cancellation);
            Err(InstallExecutionError::DevelopmentBuildDenied)
        }

        #[cfg(not(feature = "non-elevated-tests"))]
        {
            Self::execute_plan(intent, context, backend, reporter, cancellation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "non-elevated-tests"))]
    use std::cell::Cell;
    #[cfg(not(feature = "non-elevated-tests"))]
    use std::rc::Rc;

    use crate::core::native_install_controller::InstallOptions;
    use crate::core::ui_state::{AdvancedOptionsData, BootModeSelection, DriverAction};
    use lr_core::boot_pca::BootPcaMode;

    fn intent(mode: InstallMode) -> StartInstallIntent {
        StartInstallIntent {
            mode,
            running_in_pe: false,
            target_partition: "E:".to_string(),
            target_disk_number: 1,
            target_partition_number: 2,
            target_disk_size_bytes: 1_000_000_000_000,
            target_partition_offset_bytes: 1_048_576,
            target_partition_size_bytes: 500_000_000_000,
            target_stable_identity: lr_core::windows_storage::StableVolumeIdentity {
                extent: lr_core::windows_storage::VolumeIdentity {
                    disk_number: 1,
                    offset_bytes: 1_048_576,
                    extent_length_bytes: 500_000_000_000,
                },
                disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [1; 16] },
                partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                    partition_id: [2; 16],
                },
                device_id_hash: Some([3; 32]),
            },
            image_path: "D:\\install.wim".to_string(),
            image_backing_path: String::new(),
            volume_index: 1,
            is_system_partition: mode == InstallMode::ViaPe,
            pe_index: (mode == InstallMode::ViaPe).then_some(0),
            is_gho: false,
            options: InstallOptions {
                format_partition: true,
                repair_boot: true,
                unattended_install: true,
                export_drivers: true,
                auto_reboot: false,
                automation_shutdown_on_terminal: false,
                boot_mode: BootModeSelection::Auto,
                boot_pca_mode: BootPcaMode::Auto,
                advanced_options: AdvancedOptionsData::default(),
                driver_action: DriverAction::AutoImport,
                custom_unattend_path: String::new(),
                is_xp: false,
                is_xp_i386: false,
                run_diskpart_scripts: false,
                custom_install_plan: lr_core::custom_install::CustomInstallPlan::default(),
            },
        }
    }

    fn direct_context() -> InstallExecutionContext {
        InstallExecutionContext {
            stable_target: Some(StableTargetIdentity {
                disk_number: 2,
                partition_number: 3,
                disk_size_bytes: 2_000_000_000_000,
                partition_offset_bytes: 1_048_576,
                partition_size_bytes: 1_000_000_000_000,
                stable_volume: lr_core::windows_storage::StableVolumeIdentity {
                    extent: lr_core::windows_storage::VolumeIdentity {
                        disk_number: 2,
                        offset_bytes: 1_048_576,
                        extent_length_bytes: 1_000_000_000_000,
                    },
                    disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [1; 16] },
                    partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                        partition_id: [2; 16],
                    },
                    device_id_hash: Some([3; 32]),
                },
            }),
            bitlocker: BitLockerRequirement::Ready,
        }
    }

    fn add_selected_software(request: &mut StartInstallIntent) {
        request.options.advanced_options.preinstalled_software.push(
            lr_core::software_install::SelectedSoftwarePackage {
                id: "example".into(),
                name: "Example".into(),
                download_url: "https://example.invalid/setup.exe".into(),
                filename: "setup.exe".into(),
                silent_command: "{installer} /S".into(),
                requires_admin: true,
            },
        );
    }

    #[test]
    fn plan_derived_progress_ranges_are_contiguous_and_monotonic() {
        for mut request in [intent(InstallMode::Direct), intent(InstallMode::ViaPe)] {
            add_selected_software(&mut request);
            let context = if request.mode == InstallMode::Direct {
                direct_context()
            } else {
                InstallExecutionContext::default()
            };
            let plan = NativeInstallExecutor::build_plan(&request, &context).unwrap();
            let ranges = NativeInstallExecutor::progress_ranges(&request, &plan);
            assert_eq!(ranges.len(), plan.len());
            assert_eq!(ranges.first().unwrap().start, 0);
            assert_eq!(ranges.last().unwrap().end, 100);
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
                assert_eq!(pair[0].map(100), pair[1].map(0));
                assert!(pair[0].start <= pair[0].end);
            }
            assert!(ranges.last().unwrap().start <= ranges.last().unwrap().end);
        }
    }

    #[test]
    fn pe_hosted_direct_software_download_is_a_post_image_phase_without_regression() {
        let mut request = intent(InstallMode::Direct);
        request.running_in_pe = true;
        add_selected_software(&mut request);
        let plan = NativeInstallExecutor::build_plan(&request, &direct_context()).unwrap();
        assert!(!plan.contains(&InstallExecutionPhase::PreparePreinstalledSoftware));
        let image = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::ApplyWimImage)
            .unwrap();
        let boot = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::RepairBoot)
            .unwrap();
        let software = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::StageDirectPreinstalledSoftware)
            .unwrap();
        let advanced = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::ApplyAdvancedOptions)
            .unwrap();
        assert!(image < boot && boot < software && software < advanced);
        let ranges = NativeInstallExecutor::progress_ranges(&request, &plan);
        assert!(ranges[image].end <= ranges[software].start);
        assert!(ranges[software].end > ranges[software].start);
    }

    #[test]
    fn desktop_direct_downloads_before_write_and_stages_after_boot() {
        let mut request = intent(InstallMode::Direct);
        add_selected_software(&mut request);
        let plan = NativeInstallExecutor::build_plan(&request, &direct_context()).unwrap();
        let prepare = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::PreparePreinstalledSoftware)
            .unwrap();
        let format = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::FormatTarget)
            .unwrap();
        let boot = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::RepairBoot)
            .unwrap();
        let stage = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::StageDirectPreinstalledSoftware)
            .unwrap();
        assert!(prepare < format && boot < stage);
    }

    #[test]
    fn stable_target_identity_rejects_reused_numbers_with_changed_geometry() {
        let identity = direct_context().stable_target.unwrap();
        assert!(identity.matches_components(
            Some(2),
            Some(3),
            Some(2_000_000_000_000),
            Some(1_048_576),
            Some(1_000_000_000_000),
        ));
        assert!(!identity.matches_components(
            Some(2),
            Some(3),
            Some(2_000_000_000_000),
            Some(1_048_576),
            Some(999_999_995_904),
        ));
        assert!(identity.matches_components(
            Some(2),
            Some(99),
            Some(3_000_000_000_000),
            Some(1_048_576),
            Some(1_000_000_000_000),
        ));
        assert!(!identity.matches_components(
            Some(2),
            Some(3),
            Some(2_000_000_000_000),
            Some(2_097_152),
            Some(1_000_000_000_000),
        ));
        assert!(!identity.matches_components(
            None,
            Some(3),
            Some(2_000_000_000_000),
            Some(1_048_576),
            Some(1_000_000_000_000),
        ));
    }

    #[test]
    fn direct_plan_preflights_pca_before_first_mutation() {
        let plan =
            NativeInstallExecutor::build_plan(&intent(InstallMode::Direct), &direct_context())
                .unwrap();
        let pca = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::VerifyPcaBeforeDiskWrite)
            .unwrap();
        let first_write = plan
            .iter()
            .position(|phase| phase.mutates_machine())
            .unwrap();
        assert!(pca < first_write);
        assert!(plan.contains(&InstallExecutionPhase::ApplyWimImage));
        assert!(!plan.contains(&InstallExecutionPhase::ApplyGhostImage));
    }

    #[test]
    fn direct_plan_exports_requested_drivers_before_target_writes() {
        let plan =
            NativeInstallExecutor::build_plan(&intent(InstallMode::Direct), &direct_context())
                .unwrap();
        let export = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::ExportHostDrivers)
            .unwrap();
        let format = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::FormatTarget)
            .unwrap();
        assert!(export < format);
    }

    #[test]
    fn direct_plan_verifies_source_before_any_target_write() {
        let plan =
            NativeInstallExecutor::build_plan(&intent(InstallMode::Direct), &direct_context())
                .unwrap();
        let verify = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::VerifySourceImage)
            .unwrap();
        let format = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::FormatTarget)
            .unwrap();
        assert!(verify < format);
    }

    #[test]
    fn xp_text_mode_without_boot_repair_is_rejected_before_planning() {
        for mode in [InstallMode::Direct, InstallMode::ViaPe] {
            let mut request = intent(mode);
            request.options.is_xp_i386 = true;
            request.options.repair_boot = false;
            assert_eq!(
                NativeInstallExecutor::build_plan(&request, &direct_context()),
                Err(InstallExecutionError::XpTextModeRequiresBootRepair)
            );
        }
    }

    #[test]
    fn direct_gho_and_xp_paths_are_mutually_exclusive() {
        let mut gho = intent(InstallMode::Direct);
        gho.is_gho = true;
        let gho_plan = NativeInstallExecutor::build_plan(&gho, &direct_context()).unwrap();
        assert!(gho_plan.contains(&InstallExecutionPhase::ApplyGhostImage));
        assert!(!gho_plan.contains(&InstallExecutionPhase::ApplyWimImage));

        let mut xp = intent(InstallMode::Direct);
        xp.options.is_xp = true;
        xp.options.is_xp_i386 = true;
        let xp_plan = NativeInstallExecutor::build_plan(&xp, &direct_context()).unwrap();
        assert!(xp_plan.contains(&InstallExecutionPhase::ApplyXpTextModeSource));
        assert!(!xp_plan.contains(&InstallExecutionPhase::ProcessDrivers));
        assert!(!xp_plan.contains(&InstallExecutionPhase::RepairBoot));
    }

    #[test]
    fn via_pe_plan_keeps_staging_and_config_order() {
        let plan = NativeInstallExecutor::build_plan(
            &intent(InstallMode::ViaPe),
            &InstallExecutionContext::default(),
        )
        .unwrap();
        let boot = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::InstallPeBootEntry)
            .unwrap();
        let verify = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::VerifySourceImage)
            .unwrap();
        let copy = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::CopySourceImage)
            .unwrap();
        let config = plan
            .iter()
            .position(|phase| *phase == InstallExecutionPhase::WritePeInstallConfig)
            .unwrap();
        assert!(verify < copy && copy < config && config < boot);
    }

    #[test]
    fn via_pe_commit_state_honors_only_pre_boundary_cancellation() {
        let mut state = ViaPeCommitState::default();
        assert!(state.cancellable());
        assert!(state.should_honor_cancellation(true));

        state.enter_phase(InstallMode::ViaPe, InstallExecutionPhase::StageUserDrivers);
        assert!(state.should_honor_cancellation(true));

        state.enter_phase(
            InstallMode::ViaPe,
            InstallExecutionPhase::WritePeInstallConfig,
        );
        assert!(!state.cancellable());
        assert!(!state.should_honor_cancellation(true));

        for phase in [
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::ReadyToRebootIntoPe,
        ] {
            state.enter_phase(InstallMode::ViaPe, phase);
            assert!(!state.should_honor_cancellation(true));
            assert!(!state.cancellable());
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    #[test]
    fn via_pe_commit_boundary_ignores_late_cancellation_and_marks_ui_noncancellable() {
        struct CancellingBackend {
            cancelled: Rc<Cell<bool>>,
            phases: Vec<InstallExecutionPhase>,
        }

        impl InstallExecutionBackend for CancellingBackend {
            fn execute_phase(
                &mut self,
                _: &StartInstallIntent,
                _: &InstallExecutionContext,
                phase: InstallExecutionPhase,
                _: &mut dyn InstallExecutionReporter,
                _: &dyn InstallCancellation,
            ) -> Result<(), InstallBackendError> {
                self.phases.push(phase);
                if phase == InstallExecutionPhase::WritePeInstallConfig {
                    self.cancelled.set(true);
                }
                Ok(())
            }
        }

        let cancelled = Rc::new(Cell::new(false));
        let mut backend = CancellingBackend {
            cancelled: cancelled.clone(),
            phases: Vec::new(),
        };
        let mut started = Vec::new();
        let mut reporter = |event| {
            if let InstallExecutionEvent::PhaseStarted {
                phase, cancellable, ..
            } = event
            {
                started.push((phase, cancellable));
            }
        };
        let cancellation = || cancelled.get();

        assert_eq!(
            NativeInstallExecutor::execute(
                &intent(InstallMode::ViaPe),
                &InstallExecutionContext::default(),
                &mut backend,
                &mut reporter,
                &cancellation,
            ),
            Ok(InstallExecutionOutcome::ReadyToRebootIntoPe)
        );
        assert!(backend
            .phases
            .contains(&InstallExecutionPhase::InstallPeBootEntry));
        assert!(backend
            .phases
            .contains(&InstallExecutionPhase::ReadyToRebootIntoPe));
        for phase in [
            InstallExecutionPhase::WritePeInstallConfig,
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::ReadyToRebootIntoPe,
        ] {
            assert_eq!(
                started.iter().find(|(candidate, _)| *candidate == phase),
                Some(&(phase, false))
            );
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    #[test]
    fn via_pe_cancellation_before_commit_never_writes_handoff_config() {
        struct CancellingBackend {
            cancelled: Rc<Cell<bool>>,
            phases: Vec<InstallExecutionPhase>,
        }

        impl InstallExecutionBackend for CancellingBackend {
            fn execute_phase(
                &mut self,
                _: &StartInstallIntent,
                _: &InstallExecutionContext,
                phase: InstallExecutionPhase,
                _: &mut dyn InstallExecutionReporter,
                _: &dyn InstallCancellation,
            ) -> Result<(), InstallBackendError> {
                self.phases.push(phase);
                if phase == InstallExecutionPhase::StageUserDrivers {
                    self.cancelled.set(true);
                }
                Ok(())
            }
        }

        let cancelled = Rc::new(Cell::new(false));
        let mut backend = CancellingBackend {
            cancelled: cancelled.clone(),
            phases: Vec::new(),
        };
        let mut reporter = |_: InstallExecutionEvent| {};
        let cancellation = || cancelled.get();

        assert_eq!(
            NativeInstallExecutor::execute(
                &intent(InstallMode::ViaPe),
                &InstallExecutionContext::default(),
                &mut backend,
                &mut reporter,
                &cancellation,
            ),
            Err(InstallExecutionError::Cancelled)
        );
        assert!(!backend
            .phases
            .contains(&InstallExecutionPhase::WritePeInstallConfig));
        assert!(!backend
            .phases
            .contains(&InstallExecutionPhase::InstallPeBootEntry));
    }

    #[test]
    fn unsafe_runtime_facts_fail_closed() {
        assert_eq!(
            NativeInstallExecutor::build_plan(
                &intent(InstallMode::Direct),
                &InstallExecutionContext::default()
            ),
            Err(InstallExecutionError::MissingStableTarget)
        );
        let locked = InstallExecutionContext {
            stable_target: direct_context().stable_target,
            bitlocker: BitLockerRequirement::UnlockRequired,
        };
        assert_eq!(
            NativeInstallExecutor::build_plan(&intent(InstallMode::Direct), &locked),
            Err(InstallExecutionError::BitLockerUnlockRequired)
        );
    }

    #[test]
    fn user_messages_are_localized_by_error_category_without_losing_log_context() {
        assert_eq!(
            InstallExecutionError::MissingStableTarget.user_message(),
            crate::tr!("目标分区状态已变化，安装已在写入前停止。请刷新分区后重试。")
        );
        assert_eq!(
            InstallExecutionError::BitLockerUnlockRequired.user_message(),
            crate::tr!("目标分区已被 BitLocker 锁定。请先解锁分区，再重新开始安装。")
        );

        let error = InstallExecutionError::Backend {
            phase: InstallExecutionPhase::ApplyWimImage,
            source: InstallBackendError::new(
                "wim_apply_failed",
                "diagnostic-only-detail-0x80070005",
            ),
        };
        assert_eq!(
            error.user_message(),
            crate::tr!("释放系统镜像失败，安装已停止。")
        );
        assert!(!error.user_message().contains("diagnostic-only-detail"));
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("wim_apply_failed"));
        assert!(diagnostic.contains("diagnostic-only-detail-0x80070005"));
        assert!(diagnostic.contains("ApplyWimImage"));

        let image_error = InstallExecutionError::Backend {
            phase: InstallExecutionPhase::VerifySourceImage,
            source: InstallBackendError::new(
                "source_image_verification_failed",
                "private-source-path-and-low-level-detail",
            ),
        };
        let image_message = image_error.user_message();
        assert!(image_message.contains("IMG_VERIFY_FAILED"));
        assert!(image_message.contains(&crate::tr!("可能原因")));
        assert!(!image_message.contains("private-source-path"));
        assert!(!image_message.contains("source_image_verification_failed"));

        let storage_error = InstallExecutionError::Backend {
            phase: InstallExecutionPhase::VerifyPcaBeforeDiskWrite,
            source: InstallBackendError::new(
                "storage_driver_package_verification",
                "private-package-path",
            ),
        };
        let storage_message = storage_error.user_message();
        assert!(storage_message.contains(&crate::tr!("启动存储控制器驱动检查失败")));
        assert!(!storage_message.contains("private-package-path"));
        assert!(!storage_message.contains("PCA"));

        let fragmented = InstallExecutionError::Backend {
            phase: InstallExecutionPhase::SelectDataPartition,
            source: InstallBackendError::new(
                "no_data_partition",
                "aggregate free bytes are diagnostic-only",
            ),
        };
        let fragmented_message = fragmented.user_message();
        assert!(fragmented_message.contains(&crate::tr!("多个分区末尾的空闲区彼此不连续")));
        assert!(fragmented_message.contains(&crate::tr!("不会把磁盘转换为动态跨区卷")));
        assert!(!fragmented_message.contains("diagnostic-only"));
    }

    #[test]
    fn every_install_phase_has_a_nonempty_user_failure_boundary() {
        let phases = [
            InstallExecutionPhase::InspectBitLocker,
            InstallExecutionPhase::AwaitBitLockerDecryption,
            InstallExecutionPhase::VerifyPcaBeforeDiskWrite,
            InstallExecutionPhase::ResolveStableTarget,
            InstallExecutionPhase::RunDiskpartScripts,
            InstallExecutionPhase::ResolveTargetAfterDiskpart,
            InstallExecutionPhase::PreparePreinstalledSoftware,
            InstallExecutionPhase::FormatTarget,
            InstallExecutionPhase::ExportHostDrivers,
            InstallExecutionPhase::ApplyXpTextModeSource,
            InstallExecutionPhase::ApplyGhostImage,
            InstallExecutionPhase::ApplyWimImage,
            InstallExecutionPhase::ProcessDrivers,
            InstallExecutionPhase::RepairBoot,
            InstallExecutionPhase::StageDirectPreinstalledSoftware,
            InstallExecutionPhase::ApplyAdvancedOptions,
            InstallExecutionPhase::FinishDirectInstall,
            InstallExecutionPhase::VerifyPeEnvironment,
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::SelectDataPartition,
            InstallExecutionPhase::PersistPcaCompatibilityPackage,
            InstallExecutionPhase::ExportDriversToPeData,
            InstallExecutionPhase::VerifySourceImage,
            InstallExecutionPhase::CopySourceImage,
            InstallExecutionPhase::StagePreinstalledSoftware,
            InstallExecutionPhase::StageUefiSeven,
            InstallExecutionPhase::StageUserDrivers,
            InstallExecutionPhase::WritePeInstallConfig,
            InstallExecutionPhase::ReadyToRebootIntoPe,
        ];
        for phase in phases {
            let message = InstallExecutionError::Backend {
                phase,
                source: InstallBackendError::new("secret-code", "secret-detail"),
            }
            .user_message();
            assert!(!message.trim().is_empty(), "missing message for {phase:?}");
            assert!(!message.contains("secret-code"));
            assert!(!message.contains("secret-detail"));
        }
    }

    #[test]
    fn every_supported_install_phase_stops_on_injected_api_error() {
        struct FailAtPhase {
            fail_at: InstallExecutionPhase,
            code: &'static str,
            detail: &'static str,
            observed: Vec<InstallExecutionPhase>,
        }

        impl InstallExecutionBackend for FailAtPhase {
            fn execute_phase(
                &mut self,
                _: &StartInstallIntent,
                _: &InstallExecutionContext,
                phase: InstallExecutionPhase,
                _: &mut dyn InstallExecutionReporter,
                _: &dyn InstallCancellation,
            ) -> Result<(), InstallBackendError> {
                self.observed.push(phase);
                if phase == self.fail_at {
                    return Err(InstallBackendError::new(self.code, self.detail));
                }
                Ok(())
            }
        }

        let mut direct = intent(InstallMode::Direct);
        add_selected_software(&mut direct);
        let awaiting_decryption = InstallExecutionContext {
            stable_target: direct_context().stable_target,
            bitlocker: BitLockerRequirement::AwaitDecryption,
        };

        let mut ghost = intent(InstallMode::Direct);
        ghost.is_gho = true;

        let mut xp = intent(InstallMode::Direct);
        xp.options.is_xp = true;
        xp.options.is_xp_i386 = true;

        let mut via_pe = intent(InstallMode::ViaPe);
        add_selected_software(&mut via_pe);
        via_pe.options.advanced_options.win7_uefi_patch = true;

        let scenarios = [
            (direct, awaiting_decryption),
            (ghost, direct_context()),
            (xp, direct_context()),
            (via_pe, InstallExecutionContext::default()),
        ];
        let modeled_api_errors = [
            (
                "modeled_access_denied",
                "private-path ERROR_ACCESS_DENIED 5",
            ),
            (
                "modeled_sharing_violation",
                "private-path ERROR_SHARING_VIOLATION 32",
            ),
            ("modeled_disk_full", "private-path ERROR_DISK_FULL 112"),
            ("modeled_timeout", "private-path WAIT_TIMEOUT 258"),
            (
                "modeled_device_removed",
                "private-path ERROR_DEVICE_NOT_CONNECTED 1167",
            ),
            ("modeled_com_failure", "private-path E_FAIL 0x80004005"),
        ];
        let mut covered = Vec::new();

        for (request, context) in scenarios {
            let plan = NativeInstallExecutor::build_plan(&request, &context).unwrap();
            for (failure_offset, fail_at) in plan.iter().copied().enumerate() {
                for (code, detail) in modeled_api_errors {
                    let mut backend = FailAtPhase {
                        fail_at,
                        code,
                        detail,
                        observed: Vec::new(),
                    };
                    let mut events = Vec::new();
                    let mut reporter = |event| events.push(event);
                    let cancellation = || false;
                    let result = NativeInstallExecutor::execute_plan(
                        &request,
                        &context,
                        &mut backend,
                        &mut reporter,
                        &cancellation,
                    );

                    let expected = InstallExecutionError::Backend {
                        phase: fail_at,
                        source: InstallBackendError::new(code, detail),
                    };
                    assert_eq!(result, Err(expected.clone()), "{code} at {fail_at:?}");
                    assert_eq!(
                        backend.observed,
                        plan[..=failure_offset],
                        "a later phase ran after {code} at {fail_at:?}"
                    );

                    let started = events
                        .iter()
                        .filter_map(|event| match event {
                            InstallExecutionEvent::PhaseStarted { phase, .. } => Some(*phase),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    let completed = events
                        .iter()
                        .filter_map(|event| match event {
                            InstallExecutionEvent::PhaseCompleted { phase, .. } => Some(*phase),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(started, plan[..=failure_offset]);
                    assert_eq!(completed, plan[..failure_offset]);
                    assert!(!events
                        .iter()
                        .any(|event| matches!(event, InstallExecutionEvent::Completed(_))));

                    let user_message = expected.user_message();
                    assert!(!user_message.trim().is_empty());
                    assert!(!user_message.contains(code));
                    assert!(!user_message.contains("private-path"));
                    assert!(!user_message.contains(detail));
                    let diagnostic = expected.to_string();
                    assert!(diagnostic.contains(&format!("{fail_at:?}")));
                    assert!(diagnostic.contains(code));
                    assert!(diagnostic.contains(detail));
                }

                if !covered.contains(&fail_at) {
                    covered.push(fail_at);
                }
            }
        }

        // The two DiskPart-named phases are intentionally excluded: they only deserialize old
        // state and new intents are forbidden from enabling script execution.
        let supported_phases = [
            InstallExecutionPhase::InspectBitLocker,
            InstallExecutionPhase::AwaitBitLockerDecryption,
            InstallExecutionPhase::VerifyPcaBeforeDiskWrite,
            InstallExecutionPhase::ResolveStableTarget,
            InstallExecutionPhase::PreparePreinstalledSoftware,
            InstallExecutionPhase::FormatTarget,
            InstallExecutionPhase::ExportHostDrivers,
            InstallExecutionPhase::ApplyXpTextModeSource,
            InstallExecutionPhase::ApplyGhostImage,
            InstallExecutionPhase::ApplyWimImage,
            InstallExecutionPhase::ProcessDrivers,
            InstallExecutionPhase::RepairBoot,
            InstallExecutionPhase::StageDirectPreinstalledSoftware,
            InstallExecutionPhase::ApplyAdvancedOptions,
            InstallExecutionPhase::FinishDirectInstall,
            InstallExecutionPhase::VerifyPeEnvironment,
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::SelectDataPartition,
            InstallExecutionPhase::PersistPcaCompatibilityPackage,
            InstallExecutionPhase::ExportDriversToPeData,
            InstallExecutionPhase::VerifySourceImage,
            InstallExecutionPhase::CopySourceImage,
            InstallExecutionPhase::StagePreinstalledSoftware,
            InstallExecutionPhase::StageUefiSeven,
            InstallExecutionPhase::StageUserDrivers,
            InstallExecutionPhase::WritePeInstallConfig,
            InstallExecutionPhase::ReadyToRebootIntoPe,
        ];
        for phase in supported_phases {
            assert!(
                covered.contains(&phase),
                "phase lacks fault injection: {phase:?}"
            );
        }
        assert_eq!(covered.len(), supported_phases.len());
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_never_calls_backend() {
        struct PanicBackend;
        impl InstallExecutionBackend for PanicBackend {
            fn execute_phase(
                &mut self,
                _: &StartInstallIntent,
                _: &InstallExecutionContext,
                _: InstallExecutionPhase,
                _: &mut dyn InstallExecutionReporter,
                _: &dyn InstallCancellation,
            ) -> Result<(), InstallBackendError> {
                panic!("backend must not run in a development build")
            }
        }

        let mut backend = PanicBackend;
        let mut reporter = |_: InstallExecutionEvent| {};
        let cancellation = || false;
        assert_eq!(
            NativeInstallExecutor::execute(
                &intent(InstallMode::Direct),
                &direct_context(),
                &mut backend,
                &mut reporter,
                &cancellation,
            ),
            Err(InstallExecutionError::DevelopmentBuildDenied)
        );
    }
}
