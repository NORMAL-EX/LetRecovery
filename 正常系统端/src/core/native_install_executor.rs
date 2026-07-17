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
/// letters.  Direct installs therefore require the disk/partition pair before
/// the backend is allowed to mutate the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StableTargetIdentity {
    pub disk_number: u32,
    pub partition_number: u32,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    FormatTarget,
    ExportHostDrivers,
    ApplyXpTextModeSource,
    ApplyGhostImage,
    ApplyWimImage,
    ProcessDrivers,
    RepairBoot,
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
    StageUefiSeven,
    StageUserDrivers,
    WritePeInstallConfig,
    ReadyToRebootIntoPe,
}

impl InstallExecutionPhase {
    pub const fn mutates_machine(self) -> bool {
        !matches!(
            self,
            Self::InspectBitLocker
                | Self::AwaitBitLockerDecryption
                | Self::VerifyPcaBeforeDiskWrite
                | Self::ResolveStableTarget
                | Self::VerifyPeEnvironment
                | Self::VerifySourceImage
        )
    }

    /// Maps per-phase progress onto a duration-weighted overall band. WIM/GHO/XP application and
    /// source copying dominate elapsed time; short safety checks and finalization intentionally do
    /// not receive equal shares merely because they are separate phases.
    pub const fn weighted_overall_progress(self, phase_progress: u8) -> u8 {
        let (start, end) = match self {
            Self::InspectBitLocker => (0u8, 1u8),
            Self::AwaitBitLockerDecryption => (1, 3),
            Self::VerifyPcaBeforeDiskWrite => (1, 3),
            Self::ResolveStableTarget => (3, 4),
            Self::RunDiskpartScripts => (4, 6),
            Self::ResolveTargetAfterDiskpart => (6, 7),
            Self::FormatTarget => (7, 10),
            Self::ExportHostDrivers => (10, 14),
            Self::ApplyXpTextModeSource | Self::ApplyGhostImage | Self::ApplyWimImage => (14, 84),
            Self::ProcessDrivers => (84, 90),
            Self::RepairBoot => (90, 96),
            Self::ApplyAdvancedOptions => (96, 99),
            Self::FinishDirectInstall => (99, 100),
            Self::VerifyPeEnvironment => (3, 5),
            Self::InstallPeBootEntry => (5, 8),
            Self::SelectDataPartition => (8, 9),
            Self::PersistPcaCompatibilityPackage => (9, 12),
            Self::ExportDriversToPeData => (12, 20),
            Self::VerifySourceImage => (20, 30),
            Self::CopySourceImage => (30, 86),
            Self::StageUefiSeven | Self::StageUserDrivers => (86, 93),
            Self::WritePeInstallConfig => (93, 98),
            Self::ReadyToRebootIntoPe => (98, 100),
        };
        let progress = if phase_progress > 100 {
            100
        } else {
            phase_progress
        };
        let span = end.saturating_sub(start) as u16;
        let value = start as u16 + span * progress as u16 / 100;
        if value > 100 {
            100
        } else {
            value as u8
        }
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
    Cancelled,
    Backend {
        phase: InstallExecutionPhase,
        source: InstallBackendError,
    },
}

impl InstallExecutionError {
    /// Returns a concise localized message suitable for the progress page.
    ///
    /// Backend `code` and `detail` are intentionally excluded here: they remain available through
    /// [`std::fmt::Display`] for logs and support diagnostics, while the user sees a stable message
    /// for the failed safety boundary.
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
            Self::Cancelled => crate::tr!("安装已取消。"),
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
            Self::ApplyAdvancedOptions => {
                crate::tr!("应用安装高级选项失败，安装已停止。")
            }
            Self::FinishDirectInstall => crate::tr!("完成系统安装时发生错误。"),
            Self::VerifyPeEnvironment | Self::InstallPeBootEntry | Self::SelectDataPartition => {
                crate::tr!("准备 PE 安装环境失败，未进入重启阶段。")
            }
            Self::VerifySourceImage => crate::tr!("系统镜像校验失败，未复制到 PE 环境。"),
            Self::CopySourceImage => crate::tr!("复制系统镜像到 PE 数据分区失败。"),
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

    fn append_direct_phases(intent: &StartInstallIntent, phases: &mut Vec<InstallExecutionPhase>) {
        phases.push(InstallExecutionPhase::ResolveStableTarget);
        if intent.options.run_diskpart_scripts {
            phases.push(InstallExecutionPhase::RunDiskpartScripts);
            phases.push(InstallExecutionPhase::ResolveTargetAfterDiskpart);
        }
        phases.push(InstallExecutionPhase::FormatTarget);

        if intent.options.is_xp_i386 {
            // XP text-mode setup owns image copying, AHCI/NVMe/USB3 integration
            // and NT5 boot preparation.  The later generic phases must not run.
            phases.push(InstallExecutionPhase::ApplyXpTextModeSource);
            phases.push(InstallExecutionPhase::FinishDirectInstall);
            return;
        }

        if intent.options.export_drivers {
            phases.push(InstallExecutionPhase::ExportHostDrivers);
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
        phases.push(InstallExecutionPhase::ApplyAdvancedOptions);
        phases.push(InstallExecutionPhase::FinishDirectInstall);
    }

    fn append_via_pe_phases(intent: &StartInstallIntent, phases: &mut Vec<InstallExecutionPhase>) {
        phases.extend([
            InstallExecutionPhase::VerifyPeEnvironment,
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::SelectDataPartition,
        ]);
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
        if intent.options.advanced_options.win7_uefi_patch {
            phases.push(InstallExecutionPhase::StageUefiSeven);
        }
        phases.extend([
            InstallExecutionPhase::StageUserDrivers,
            InstallExecutionPhase::WritePeInstallConfig,
            InstallExecutionPhase::ReadyToRebootIntoPe,
        ]);
    }

    /// Runs an already validated intent through an injected backend.
    ///
    /// No production backend is provided here yet: moving the destructive
    /// bodies out of `ui/install_progress.rs` requires a separate, reviewable
    /// change.  This prevents the native window from accidentally running a
    /// partial reimplementation while still giving it a stable message model.
    // TODO(native-install-backend): move each verified legacy operation behind
    // InstallExecutionBackend, then wire the native window to that backend.
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
            let plan = Self::build_plan(intent, context)?;
            reporter.report(InstallExecutionEvent::Started {
                total_phases: plan.len(),
            });
            for (offset, phase) in plan.iter().copied().enumerate() {
                if cancellation.is_cancelled() {
                    return Err(InstallExecutionError::Cancelled);
                }
                let index = offset + 1;
                reporter.report(InstallExecutionEvent::PhaseStarted {
                    index,
                    total: plan.len(),
                    phase,
                });
                if let Err(source) =
                    backend.execute_phase(intent, context, phase, reporter, cancellation)
                {
                    if source.code == "cancelled" {
                        return Err(InstallExecutionError::Cancelled);
                    }
                    return Err(InstallExecutionError::Backend { phase, source });
                }
                reporter.report(InstallExecutionEvent::PhaseCompleted {
                    index,
                    total: plan.len(),
                    phase,
                });
            }
            let outcome = match intent.mode {
                InstallMode::Direct => InstallExecutionOutcome::DirectInstallCompleted,
                InstallMode::ViaPe => InstallExecutionOutcome::ReadyToRebootIntoPe,
            };
            reporter.report(InstallExecutionEvent::Completed(outcome));
            Ok(outcome)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_application_owns_most_of_direct_install_progress() {
        assert_eq!(
            InstallExecutionPhase::FormatTarget.weighted_overall_progress(100),
            10
        );
        assert_eq!(
            InstallExecutionPhase::ApplyWimImage.weighted_overall_progress(0),
            14
        );
        assert_eq!(
            InstallExecutionPhase::ApplyWimImage.weighted_overall_progress(50),
            49
        );
        assert_eq!(
            InstallExecutionPhase::ApplyWimImage.weighted_overall_progress(100),
            84
        );
    }
    use crate::core::native_install_controller::InstallOptions;
    use crate::core::ui_state::{AdvancedOptionsData, BootModeSelection, DriverAction};
    use lr_core::boot_pca::BootPcaMode;

    fn intent(mode: InstallMode) -> StartInstallIntent {
        StartInstallIntent {
            mode,
            target_partition: "E:".to_string(),
            target_disk_number: 1,
            target_partition_number: 2,
            image_path: "D:\\install.wim".to_string(),
            volume_index: 1,
            is_system_partition: mode == InstallMode::ViaPe,
            selected_pe: (mode == InstallMode::ViaPe).then_some(0),
            is_gho: false,
            options: InstallOptions {
                format_partition: true,
                repair_boot: true,
                unattended_install: true,
                export_drivers: true,
                auto_reboot: false,
                boot_mode: BootModeSelection::Auto,
                boot_pca_mode: BootPcaMode::Auto,
                advanced_options: AdvancedOptionsData::default(),
                driver_action: DriverAction::AutoImport,
                custom_unattend_path: String::new(),
                is_xp: false,
                is_xp_i386: false,
                run_diskpart_scripts: false,
            },
        }
    }

    fn direct_context() -> InstallExecutionContext {
        InstallExecutionContext {
            stable_target: Some(StableTargetIdentity {
                disk_number: 2,
                partition_number: 3,
            }),
            bitlocker: BitLockerRequirement::Ready,
        }
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
        assert!(boot < verify && verify < copy && copy < config);
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
            InstallExecutionPhase::FormatTarget,
            InstallExecutionPhase::ExportHostDrivers,
            InstallExecutionPhase::ApplyXpTextModeSource,
            InstallExecutionPhase::ApplyGhostImage,
            InstallExecutionPhase::ApplyWimImage,
            InstallExecutionPhase::ProcessDrivers,
            InstallExecutionPhase::RepairBoot,
            InstallExecutionPhase::ApplyAdvancedOptions,
            InstallExecutionPhase::FinishDirectInstall,
            InstallExecutionPhase::VerifyPeEnvironment,
            InstallExecutionPhase::InstallPeBootEntry,
            InstallExecutionPhase::SelectDataPartition,
            InstallExecutionPhase::PersistPcaCompatibilityPackage,
            InstallExecutionPhase::ExportDriversToPeData,
            InstallExecutionPhase::VerifySourceImage,
            InstallExecutionPhase::CopySourceImage,
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
