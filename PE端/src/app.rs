use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::config::{ConfigFileManager, OperationType};
use crate::core::dism::DismProgress;
use crate::tr;
use crate::ui::progress::{BackupStep, InstallStep, ProgressState};
use crate::utils::reboot_pe;
use crate::workflow_journal::PeWorkflowJournal;
use crate::workflow_journal::RecoveryCheckpointSnapshot;

/// 工作线程消息
#[derive(Debug, Clone)]
pub(crate) enum WorkerMessage {
    /// 更新安装步骤
    SetInstallStep(InstallStep),
    /// 更新备份步骤
    SetBackupStep(BackupStep),
    /// 更新步骤进度
    SetProgress(u8),
    /// 更新状态消息
    SetStatus(String),
    /// Atomically publish one external-tool progress sample. DISM commonly emits a percentage and
    /// status line together; keeping them in one message halves channel pressure and prevents the
    /// UI from briefly presenting a percentage from one sample with text from another.
    SetProgressStatus { progress: u8, status: String },
    /// 标记完成
    Completed,
    /// The operation succeeded but automatic reboot is suppressed until the user reviews the
    /// described post-install warning.
    CompletedWithWarning(String),
    /// 标记失败
    Failed(String),
}

/// A worker poll runs on the Win32 UI thread. It must yield before the 16 ms animation timer is
/// starved, even when an image engine produces progress messages faster than they can be painted.
pub(crate) const MAX_WORKER_MESSAGES_PER_POLL: usize = 256;
const MAX_WORKER_POLL_SLICE: Duration = Duration::from_millis(4);

fn should_reboot_after_completion_warning(
    _interactive_auto_reboot: bool,
    automation_shutdown_on_terminal: bool,
) -> bool {
    // An interactive warning is actionable information, not a three-second splash screen. Leave
    // the completed PE session visible until the user has read it and chooses how to restart.
    // Authenticated disposable-VM automation has no reader and must still reach its terminal power
    // state without manual input.
    automation_shutdown_on_terminal
}

pub(crate) struct WorkflowSession {
    /// 进度状态
    progress_state: Arc<Mutex<ProgressState>>,
    /// 消息接收器
    message_rx: Option<Receiver<WorkerMessage>>,
    /// 是否已启动
    started: bool,
    /// Worker handle is retained so display terminal messages cannot be mistaken for the end of
    /// cleanup, delay and reboot tail work.
    worker_handle: Option<thread::JoinHandle<()>>,
    worker_finished: bool,
    terminal_message_seen: bool,
    channel_failure_reported: bool,
    /// 操作类型
    operation_type: Option<OperationType>,
    authenticated_handoff: Option<crate::core::config::AuthenticatedOperationGuard>,
    /// Durable observer for crash diagnostics. Recording failures never block
    /// the existing install, backup, or expand workflow.
    workflow_journal: Option<PeWorkflowJournal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowRecoverySnapshot {
    pub checkpoint: Option<RecoveryCheckpointSnapshot>,
    pub worker_started: bool,
    pub worker_finished: bool,
}

impl WorkflowSession {
    pub(crate) fn new_for_operation(
        operation_type: Option<OperationType>,
        authenticated_handoff: crate::core::config::AuthenticatedOperationGuard,
    ) -> Self {
        // A workflow journal may only be placed after the typed task has located its data volume
        // from the WIM-authenticated random token. The former pre-task INI scan was intentionally
        // removed: unrelated same-name files must never influence startup. Until the journal owns
        // a typed-task root, diagnostics stay disabled rather than reintroducing legacy discovery.
        let workflow_journal = None;

        let progress_state = Arc::new(Mutex::new(match operation_type {
            Some(OperationType::Install) => ProgressState::new_install(),
            Some(OperationType::Backup) => ProgressState::new_backup(),
            Some(OperationType::Expand) => ProgressState::new_expand(),
            None => ProgressState::new_install(),
        }));

        WorkflowSession {
            progress_state,
            message_rx: None,
            started: false,
            worker_handle: None,
            worker_finished: false,
            terminal_message_seen: false,
            channel_failure_reported: false,
            operation_type,
            authenticated_handoff: Some(authenticated_handoff),
            workflow_journal,
        }
    }

    /// 启动工作线程
    /// Build a message-driven preview session without starting a worker or touching the workflow
    /// journal. The UI preview uses this to exercise the same bounded receiver and state-transition
    /// path as production while remaining safe on a normal desktop.
    #[cfg(any(test, feature = "non-elevated-tests"))]
    pub(crate) fn new_message_preview(
        operation_type: OperationType,
    ) -> (Self, Sender<WorkerMessage>) {
        let progress_state = Arc::new(Mutex::new(match operation_type {
            OperationType::Install => ProgressState::new_install(),
            OperationType::Backup => ProgressState::new_backup(),
            OperationType::Expand => ProgressState::new_expand(),
        }));
        let (tx, rx) = channel();
        (
            Self {
                progress_state,
                message_rx: Some(rx),
                started: true,
                worker_handle: None,
                worker_finished: false,
                terminal_message_seen: false,
                channel_failure_reported: false,
                operation_type: Some(operation_type),
                authenticated_handoff: None,
                workflow_journal: None,
            },
            tx,
        )
    }

    pub(crate) fn start_worker(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        let (tx, rx) = channel::<WorkerMessage>();
        self.message_rx = Some(rx);

        let operation_type = self.operation_type;
        let authenticated_handoff = self.authenticated_handoff.take();

        self.worker_handle = Some(thread::spawn(move || {
            match (operation_type, authenticated_handoff) {
                (Some(OperationType::Install), Some(guard)) => {
                    execute_install_workflow(tx, guard);
                }
                (Some(OperationType::Backup), Some(guard)) => {
                    crate::workflows::execute_backup_workflow(tx, guard);
                }
                (Some(OperationType::Expand), Some(guard)) => {
                    crate::workflows::execute_expand_workflow(tx, guard);
                }
                _ => {
                    let _ = tx.send(WorkerMessage::Failed(tr!("未检测到安装或备份配置")));
                }
            }
        }));
    }

    /// 处理工作线程消息
    pub(crate) fn process_messages(&mut self) {
        let poll_started = Instant::now();
        let mut processed = 0usize;
        let mut disconnected = false;
        let mut pending_progress = None;
        let mut pending_status = None;
        if let Some(ref rx) = self.message_rx {
            loop {
                let msg = match rx.try_recv() {
                    Ok(msg) => msg,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                };
                match msg {
                    WorkerMessage::SetProgress(progress) => {
                        pending_progress = Some(progress);
                    }
                    WorkerMessage::SetStatus(status) => {
                        pending_status = Some(status);
                    }
                    WorkerMessage::SetProgressStatus { progress, status } => {
                        pending_progress = Some(progress);
                        pending_status = Some(status);
                    }
                    transition => {
                        // Preserve ordering at semantic boundaries while collapsing a flood of
                        // intermediate tool samples to the newest visible value.
                        flush_pending_worker_update(
                            &self.progress_state,
                            &mut pending_progress,
                            &mut pending_status,
                        );
                        if matches!(
                            transition,
                            WorkerMessage::Completed
                                | WorkerMessage::CompletedWithWarning(_)
                                | WorkerMessage::Failed(_)
                        ) {
                            self.terminal_message_seen = true;
                        }
                        if let Some(journal) = self.workflow_journal.as_mut() {
                            let result = match &transition {
                                WorkerMessage::SetInstallStep(step) => {
                                    journal.observe_install_step(*step)
                                }
                                WorkerMessage::SetBackupStep(step) => {
                                    journal.observe_backup_step(*step)
                                }
                                WorkerMessage::Completed => journal.complete(),
                                WorkerMessage::CompletedWithWarning(_) => journal.complete(),
                                WorkerMessage::Failed(error) => journal.fail(error),
                                WorkerMessage::SetProgress(_)
                                | WorkerMessage::SetStatus(_)
                                | WorkerMessage::SetProgressStatus { .. } => unreachable!(),
                            };
                            if let Err(error) = result {
                                log::warn!(
                                    "[CHECKPOINT] 记录工作流状态失败，将继续原流程: {}",
                                    error
                                );
                            }
                        }
                        if let Ok(mut state) = self.progress_state.lock() {
                            match transition {
                                WorkerMessage::SetInstallStep(step) => {
                                    state.set_install_step(step);
                                }
                                WorkerMessage::SetBackupStep(step) => {
                                    state.set_backup_step(step);
                                }
                                WorkerMessage::Completed => {
                                    state.mark_completed();
                                }
                                WorkerMessage::CompletedWithWarning(warning) => {
                                    state.mark_completed_with_warning(Some(warning));
                                }
                                WorkerMessage::Failed(error) => {
                                    state.mark_failed(&error);
                                }
                                WorkerMessage::SetProgress(_)
                                | WorkerMessage::SetStatus(_)
                                | WorkerMessage::SetProgressStatus { .. } => unreachable!(),
                            }
                        }
                    }
                }
                processed += 1;
                if processed >= MAX_WORKER_MESSAGES_PER_POLL
                    || poll_started.elapsed() >= MAX_WORKER_POLL_SLICE
                {
                    break;
                }
            }
        }
        flush_pending_worker_update(
            &self.progress_state,
            &mut pending_progress,
            &mut pending_status,
        );
        if disconnected && !self.terminal_message_seen && !self.channel_failure_reported {
            self.channel_failure_reported = true;
            self.terminal_message_seen = true;
            let message = tr!("工作线程异常终止");
            if let Some(journal) = self.workflow_journal.as_mut() {
                if let Err(error) = journal.fail(&message) {
                    log::warn!("[CHECKPOINT] 记录工作线程异常终止失败，将继续显示错误: {error}");
                }
            }
            if let Ok(mut state) = self.progress_state.lock() {
                state.mark_failed(&message);
            }
        }
    }

    pub(crate) fn snapshot(&self) -> ProgressState {
        self.progress_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub(crate) fn recovery_snapshot(&self) -> WorkflowRecoverySnapshot {
        WorkflowRecoverySnapshot {
            checkpoint: self
                .workflow_journal
                .as_ref()
                .map(PeWorkflowJournal::recovery_snapshot),
            worker_started: self.started,
            worker_finished: self.worker_finished,
        }
    }

    pub(crate) fn reap_worker_if_finished(&mut self) -> bool {
        if self.worker_finished {
            return true;
        }
        let finished = self
            .worker_handle
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished);
        if !finished {
            return false;
        }
        if let Some(handle) = self.worker_handle.take() {
            if handle.join().is_err() {
                log::error!("PE 工作线程在完成尾处理时发生 panic");
            }
        }
        self.worker_finished = true;
        true
    }
}

fn flush_pending_worker_update(
    progress_state: &Arc<Mutex<ProgressState>>,
    progress: &mut Option<u8>,
    status: &mut Option<String>,
) {
    if progress.is_none() && status.is_none() {
        return;
    }
    if let Ok(mut state) = progress_state.lock() {
        if let Some(progress) = progress.take() {
            state.set_step_progress(progress);
        }
        if let Some(status) = status.take() {
            state.status_message = status;
        }
    }
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn cleanup_target_private_pe_residue(target_partition: &str) -> anyhow::Result<usize> {
    let drive = target_partition.trim().trim_end_matches(['\\', '/']);
    if drive.len() != 2 || !drive.as_bytes()[0].is_ascii_alphabetic() || drive.as_bytes()[1] != b':'
    {
        anyhow::bail!("invalid target partition before private PE cleanup");
    }
    let root = std::path::PathBuf::from(format!(r"{drive}\LetRecovery_PE"));
    cleanup_private_pe_residue_root(&root)
}

fn cleanup_private_pe_residue_root(root: &std::path::Path) -> anyhow::Result<usize> {
    use anyhow::Context as _;

    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("inspect stale target PE directory"),
    };
    if !root_metadata.is_dir() || metadata_is_reparse_point(&root_metadata) {
        anyhow::bail!(
            "target private PE residue is not an ordinary directory: {}",
            root.display()
        );
    }

    let mut removed = 0usize;
    let mut retained = Vec::new();
    for entry in std::fs::read_dir(root).context("enumerate stale target PE directory")? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            retained.push(entry.file_name().to_string_lossy().into_owned());
            continue;
        };
        let product_file = matches!(name.as_str(), "pe_guid.txt" | "pe_pending.txt")
            || lr_core::handoff_auth::is_orphaned_private_pe_file_name(&name);
        if !product_file {
            retained.push(name);
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("inspect stale target PE artifact {}", path.display()))?;
        if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
            anyhow::bail!(
                "refusing to remove linked or non-file target PE artifact: {}",
                path.display()
            );
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("remove stale target PE artifact {}", path.display()))?;
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!(
                "target PE artifact remains after removal: {}",
                path.display()
            ),
        }
        removed += 1;
    }
    if std::fs::read_dir(root)?.next().is_none() {
        std::fs::remove_dir(root).context("remove empty stale target PE directory")?;
    } else if !retained.is_empty() {
        retained.sort();
        retained.truncate(8);
        log::warn!(
            "[PE INSTALL] 目标的 LetRecovery_PE 中含非任务文件，已保留: {}",
            retained.join(", ")
        );
    }
    if removed != 0 {
        log::info!(
            "[PE INSTALL] 已清理目标上次任务遗留的私有 PE 文件: count={} root={}",
            removed,
            root.display()
        );
    }
    Ok(removed)
}

fn fail_install_before_destructive_write(
    tx: &Sender<WorkerMessage>,
    task: crate::core::config::AuthenticatedOperationTask,
    reason: String,
) {
    let dual_rollback = task.install_config().ok().and_then(|config| {
        matches!(
            config.custom_install_plan,
            lr_core::custom_install::CustomInstallPlan::DualBoot(_)
        )
        .then(|| {
            task.install_target()
                .map(|(_, identity)| (config.custom_install_plan.clone(), identity))
        })
    });
    let mut rollback_errors = Vec::new();
    if let Err(error) = crate::cleanup_persistent_pe_boot_payload(task.guard()) {
        rollback_errors.push(format!("PE 启动项/私有载荷清理失败: {error:#}"));
    }
    match task.into_prewrite_cleanup_authorization() {
        Ok(auto_staging) => {
            if let Some(rollback) = dual_rollback {
                match rollback.and_then(|(plan, target)| {
                    crate::core::custom_install::rollback_dual_boot_before_write(&plan, target)
                }) {
                    Ok(_) => {}
                    Err(error) => {
                        rollback_errors.push(format!("双系统预创建卷回退失败: {error:#}"));
                    }
                }
            }
            if let Some(authorization) = auto_staging {
                if let Err(error) =
                    crate::core::disk::DiskManager::cleanup_authenticated_auto_staging(
                        &authorization,
                    )
                {
                    rollback_errors.push(format!("自动暂存卷回退失败: {error:#}"));
                }
            }
        }
        Err(error) => rollback_errors.push(format!("本次随机 marker 清理失败: {error:#}")),
    }
    let rollback_result = if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(rollback_errors.join("；")))
    };
    let message = match rollback_result {
        Ok(()) => tr!(
            "{}；尚未删除、格式化或覆盖原系统，已自动回退本次安装会话。",
            reason
        ),
        Err(rollback_error) => tr!(
            "{}；原系统尚未进入删除、格式化或覆盖阶段，但自动回退本次安装会话未能完整收束: {}",
            reason,
            rollback_error
        ),
    };
    let _ = tx.send(WorkerMessage::Failed(message));
}

fn suppress_new_install_only_options(config: &mut crate::core::config::InstallConfig) -> bool {
    let requested = config.unattended
        || !config.custom_unattend_file.is_empty()
        || !config.custom_username.is_empty()
        || config.builtin_administrator.enabled
        || config.migrate_wifi
        || config.remove_uwp_apps
        || config.disable_windows_defender
        || config.disable_reserved_storage
        || !config.preinstalled_software_config.is_empty();

    config.unattended = false;
    config.custom_unattend_file.clear();
    config.custom_username.clear();
    config.builtin_administrator.enabled = false;
    config.builtin_administrator.password.clear();
    config.migrate_wifi = false;
    config.remove_uwp_apps = false;
    config.disable_windows_defender = false;
    config.disable_reserved_storage = false;
    config.preinstalled_software_config.clear();
    requested
}

fn stage_authenticated_preinstalled_software(
    target_partition: &str,
    packages: &[lr_core::software_install::SelectedSoftwarePackage],
    artifacts: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    if packages.is_empty() {
        if !artifacts.is_empty() {
            anyhow::bail!(
                "preinstalled-software artifacts exist without an authenticated selection"
            );
        }
        return Ok(());
    }
    if artifacts.len() != packages.len() {
        anyhow::bail!("preinstalled-software artifact count changed after authentication");
    }
    let source_directory = artifacts
        .first()
        .and_then(|path| path.parent())
        .ok_or_else(|| anyhow::anyhow!("preinstalled-software source directory is missing"))?;
    if artifacts
        .iter()
        .any(|path| path.parent() != Some(source_directory))
    {
        anyhow::bail!("preinstalled-software artifacts do not share one authenticated directory");
    }
    let target_root = std::path::PathBuf::from(format!(
        "{}\\",
        target_partition.trim_end_matches(['\\', '/'])
    ));
    let destination = target_root
        .join("LetRecovery_Scripts")
        .join(lr_core::software_install::STAGING_DIRECTORY_NAME);
    if destination.exists() {
        anyhow::bail!(
            "preinstalled-software destination already exists: {}",
            destination.display()
        );
    }
    let copied = lr_core::windows_file_copy::copy_tree_verified(source_directory, &destination)?;
    if copied != packages.len() {
        anyhow::bail!(
            "preinstalled-software copy count mismatch: expected {}, copied {}",
            packages.len(),
            copied
        );
    }
    log::info!(
        "[PREINSTALLED_SOFTWARE] status=staged count={} destination={}",
        copied,
        destination.display()
    );
    Ok(())
}

/// Armed only by the authenticated CLI automation field. Declared before the terminal log
/// finalizer so Rust drops/flushed the latter first on destructive-stage failures, then asks the
/// disposable VM to power off. Success paths explicitly disarm it before rebooting into the new
/// system, where the first-logon finalizer performs the terminal shutdown after software setup.
struct AutomationFailureShutdown {
    armed: bool,
}

impl AutomationFailureShutdown {
    fn new(enabled: bool) -> Self {
        Self { armed: enabled }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AutomationFailureShutdown {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match lr_core::windows_shutdown::schedule_shutdown(
            15,
            "LetRecovery PE automation reached a terminal failure; this test machine will power off.",
        ) {
            Ok(()) => log::info!(
                "[AUTOMATION] terminal=failure action=shutdown status=accepted timeout_seconds=15"
            ),
            Err(error) => log::error!(
                "[AUTOMATION] terminal=failure action=shutdown status=failed error={error:#}"
            ),
        }
    }
}

/// 执行安装工作流
fn execute_install_workflow(
    tx: Sender<WorkerMessage>,
    authenticated_handoff: crate::core::config::AuthenticatedOperationGuard,
) {
    use crate::core::bcdedit::BootManager;
    use crate::core::disk::DiskManager;
    use crate::core::dism::Dism;
    use crate::core::ghost::Ghost;
    use crate::ui::advanced_options::apply_advanced_options;

    log::info!("========== 开始PE安装流程 ==========");
    // The move-only task is the sole authority. Converting the X: LRHC1 guard locks and hashes
    // every exact public artifact once. Later consumers keep using that held exact set; large
    // driver/XP trees are never copied into the X: RAM disk.
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::VerifyImage));
    let _ = tx.send(WorkerMessage::SetProgressStatus {
        progress: 0,
        status: tr!("正在定位本次安装卷并认证安装文件..."),
    });
    let mut last_auth_progress = u8::MAX;
    let mut authenticated_task = match authenticated_handoff.into_task_with_progress(|event| {
        use crate::core::config::TaskAuthenticationProgress;
        match event {
            TaskAuthenticationProgress::LocatingVolumes => {
                let _ = tx.send(WorkerMessage::SetStatus(tr!("正在定位本次安装卷...")));
            }
            TaskAuthenticationProgress::AuthenticatingArtifacts {
                completed_bytes,
                total_bytes,
                current_path,
            } => {
                let percent = if total_bytes == 0 {
                    100
                } else {
                    completed_bytes
                        .saturating_mul(100)
                        .saturating_div(total_bytes)
                        .min(100) as u8
                };
                if percent != last_auth_progress {
                    last_auth_progress = percent;
                    let name = std::path::Path::new(&current_path)
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or(&current_path);
                    let _ = tx.send(WorkerMessage::SetProgressStatus {
                        progress: percent,
                        status: tr!("正在认证安装文件: {} ({}%)", name, percent),
                    });
                }
            }
            TaskAuthenticationProgress::Finalizing => {
                let _ = tx.send(WorkerMessage::SetProgressStatus {
                    progress: 100,
                    status: tr!("安装文件认证完成"),
                });
            }
        }
    }) {
        Ok(task) => task,
        Err(e) => {
            let _ = tx.send(WorkerMessage::Failed(tr!("认证安装任务失败: {}", e)));
            return;
        }
    };
    macro_rules! fail_prewrite {
        ($message:expr) => {{
            fail_install_before_destructive_write(&tx, authenticated_task, $message);
            return;
        }};
    }
    let mut config = match authenticated_task.install_config() {
        Ok(config) => config.clone(),
        Err(error) => {
            fail_prewrite!(tr!("安装任务类型无效: {}", error));
        }
    };
    #[cfg(feature = "ci-automation")]
    if let Err(error) = crate::register_ci_authenticated_install_context(&config.session_id) {
        fail_prewrite!(tr!(
            "CI 安装故障注入记录无法绑定到本次认证会话，尚未写入目标: {}",
            error
        ));
    }
    let mut automation_failure_shutdown =
        AutomationFailureShutdown::new(config.automation_shutdown_on_terminal);
    let mut private_wifi_profile = match authenticated_task.private_wifi_profile_bytes() {
        Ok(Some(bytes)) => Some(zeroize::Zeroizing::new(bytes.to_vec())),
        Ok(None) => None,
        Err(error) => {
            fail_prewrite!(tr!("无法认证本次 Wi-Fi 迁移配置: {}", error));
        }
    };
    let mut selected_preinstalled_software = match config.selected_preinstalled_software() {
        Ok(packages) => packages,
        Err(error) => {
            fail_prewrite!(tr!("无法读取本次预装软件选择: {}", error));
        }
    };
    let required_online_cleanup = config.remove_uwp_apps
        || config.disable_windows_defender
        || !selected_preinstalled_software.is_empty();
    if let Err(error) = lr_core::unattend_command::validate_required_builtin_unattend(
        required_online_cleanup,
        config.unattended,
        !config.custom_unattend_file.is_empty(),
        config.is_gho || config.is_xp || config.is_xp_i386,
    ) {
        use lr_core::unattend_command::RequiredBuiltinUnattendError as Error;
        let message = match error {
            Error::UnattendedDisabled => {
                tr!("预装软件、移除预装应用或移除 Windows 安全中心需要启用 LetRecovery 内置无人值守安装。")
            }
            Error::CustomUnattend => {
                tr!("预装软件、移除预装应用或移除 Windows 安全中心不能与自定义应答文件同时使用。")
            }
            Error::UnsupportedSource => {
                tr!("预装软件、移除预装应用或移除 Windows 安全中心不支持 GHO/GHS 或 XP 文本模式来源。")
            }
        };
        fail_prewrite!(message);
    }
    let (mut target_partition, mut expected_target) = match authenticated_task.install_target() {
        Ok((partition, identity)) => (partition.to_owned(), identity),
        Err(error) => {
            fail_prewrite!(tr!(
                "无法找到与本次随机标记完全匹配的唯一安装分区: {}",
                error
            ));
        }
    };
    let mut full_disk_staging_cleanup = None;
    let public_data_root = authenticated_task.data_volume_root().to_path_buf();
    let data_partition = public_data_root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_owned();
    let public_data_dir = public_data_root.join("LetRecovery_Data");
    if config.is_xp_i386 && !config.repair_boot {
        fail_prewrite!(tr!(
            "XP/2003 文本模式安装必须启用“添加引导”，尚未写入目标分区。"
        ));
    }
    log::info!("数据分区: {}", data_partition);
    let _ = tx.send(WorkerMessage::SetStatus(tr!(
        "数据分区: {}",
        data_partition
    )));

    // 切换到正常系统端选定的镜像引擎（随重启传入），使 PE 端使用相同引擎
    crate::copy_desktop_install_log_into_pe(&data_partition, &config.session_id);
    lr_core::set_active_engine(lr_core::WimEngine::from_u8(config.wim_engine));

    log::info!("目标分区: {}", config.target_partition);
    log::info!("镜像文件: {}", config.image_path);
    let pca_compat_target_arch =
        if config.pca_compat_target_build == 0 && config.pca_compat_target_architecture == 0 {
            "未提供"
        } else {
            match config.pca_compat_target_architecture {
                0 => "x86",
                9 => "x64",
                12 => "ARM64",
                _ => "未知",
            }
        };
    log::info!(
        "[诊断环境] PE 安装任务: target={} | image_file={} | volume_index={} | format={} | boot_mode={} | boot_signature={:?} | pca_compat_target_build={} | pca_compat_target_arch={}",
        config.target_partition,
        config.image_path,
        config.volume_index,
        if config.is_xp_i386 {
            "XP-I386"
        } else if config.is_gho {
            "GHO/GHS"
        } else {
            "WIM/ESD/SWM"
        },
        match config.boot_mode {
            1 => "UEFI",
            2 => "Legacy",
            _ => "Auto",
        },
        config.boot_pca_mode,
        config.pca_compat_target_build,
        pca_compat_target_arch,
    );

    let data_dir = match authenticated_task.install_data_dir() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(error) => {
            fail_prewrite!(tr!("解析认证安装数据目录失败: {}", error));
        }
    };
    let preserved_driver_artifacts = match authenticated_task
        .install_artifact_paths(lr_core::handoff_manifest::ArtifactRole::PreservedDriver)
    {
        Ok(paths) => paths,
        Err(error) => {
            fail_prewrite!(tr!("读取认证驱动清单失败: {}", error));
        }
    };
    let preserved_driver_infs = preserved_driver_artifacts
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("inf"))
        })
        .cloned()
        .collect::<Vec<_>>();
    let preserved_driver_cabs = preserved_driver_artifacts
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("cab"))
        })
        .cloned()
        .collect::<Vec<_>>();
    let user_driver_artifacts = match authenticated_task
        .install_artifact_paths(lr_core::handoff_manifest::ArtifactRole::UserDriver)
    {
        Ok(paths) => paths,
        Err(error) => {
            fail_prewrite!(tr!("读取认证用户驱动清单失败: {}", error));
        }
    };
    let preinstalled_software_artifacts = match authenticated_task
        .install_artifact_paths(lr_core::handoff_manifest::ArtifactRole::PreinstalledSoftware)
    {
        Ok(paths) => paths,
        Err(error) => {
            fail_prewrite!(tr!("读取认证预装软件清单失败: {}", error));
        }
    };
    let update_package_artifacts = match authenticated_task
        .install_artifact_paths(lr_core::handoff_manifest::ArtifactRole::UpdatePackage)
    {
        Ok(paths) => paths,
        Err(error) => {
            fail_prewrite!(tr!("读取认证更新包清单失败: {}", error));
        }
    };
    let mut image_path = match authenticated_task.install_source_path() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(error) => {
            fail_prewrite!(tr!("解析认证安装源失败: {}", error));
        }
    };

    if !std::path::Path::new(&image_path).exists() {
        fail_prewrite!(tr!("镜像文件不存在: {}", image_path));
    }

    // The task already owns the exact manifest image handle. Never rediscover a span set from
    // the public directory here: doing so could incorporate an unmanifested SWM/GHS sibling.
    let locked_xp_source = if config.is_xp_i386 {
        match lr_core::install_source_lock::LockedInstallTree::acquire(std::path::Path::new(
            &image_path,
        )) {
            Ok(locked) => {
                image_path = locked.selected_path().to_string_lossy().into_owned();
                Some(locked)
            }
            Err(error) => {
                fail_prewrite!(tr!("无法锁定 XP/2003 目录源，已停止写盘: {}", error));
            }
        }
    } else {
        None
    };

    log::info!("完整镜像路径: {}", image_path);

    // Step 0: 校验镜像完整性（WIM/ESD）。放在格式化之前——镜像损坏就提前失败，
    // 不会白白格式化目标盘，也能给出明确“镜像损坏”而不是释放到一半才崩。
    // GHO 不是 WIM，跳过 wimlib 校验。
    let xp_custom_sif = if config.is_xp_i386 && !config.custom_unattend_file.is_empty() {
        match ConfigFileManager::resolve_staged_file(&data_dir, &config.custom_unattend_file) {
            Ok(path) => Some(path),
            Err(error) => {
                fail_prewrite!(tr!("自定义 XP 应答文件名无效: {}", error));
            }
        }
    } else {
        None
    };
    if config.is_xp_i386 {
        if let Err(error) =
            lr_core::xp_i386::validate_i386_source(std::path::Path::new(&image_path))
        {
            fail_prewrite!(tr!("XP/2003 安装源校验失败: {}", error));
        }
    }

    if config.is_gho {
        let ghost = Ghost::new();
        if !ghost.is_available() {
            fail_prewrite!(tr!("Ghost工具不可用"));
        }
        if let Err(error) = ghost.verify_image_integrity(&image_path) {
            fail_prewrite!(tr!("GHO 镜像预检失败: {}", error));
        }
        log::info!("[PE安装] GHO 镜像预检通过，尚未修改目标分区");
    } else if !config.is_xp_i386 {
        let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::VerifyImage));
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "正在校验系统镜像完整性（可能需要几分钟）..."
        )));
        log::info!("[PE安装] 开始校验镜像: {}", image_path);

        let (verify_tx, verify_rx) = channel::<DismProgress>();
        let tx_v = tx.clone();
        let verify_handle = thread::spawn(move || {
            while let Ok(progress) = verify_rx.recv() {
                let _ = tx_v.send(WorkerMessage::SetProgressStatus {
                    progress: progress.percentage,
                    status: progress.status,
                });
            }
        });

        let verify_result = Dism::new().verify_image(&image_path, Some(verify_tx));
        let _ = verify_handle.join();

        if let Err(e) = verify_result {
            log::error!("[PE安装] 镜像校验失败: {}", e);
            if e.is_out_of_memory() {
                fail_prewrite!(tr!(
                    "镜像校验因可用内存不足而无法完成（{}）。请重启 PE 后重试，或为设备提供更多内存。",
                    e
                ));
            } else {
                fail_prewrite!(tr!(
                    "镜像校验失败：镜像可能已损坏或不完整（{}）。请重新获取镜像后重试。",
                    e
                ));
            }
        }
        log::info!("[PE安装] 镜像校验通过");
        let _ = tx.send(WorkerMessage::SetProgress(100));
    } else {
        log::info!("[PE安装] GHO 镜像，跳过 wimlib 校验");
    }

    // PCA/EFI validation only protects a later boot write. When the user
    // explicitly disabled boot repair, neither validate nor stage boot assets.
    let boot_preflight = if !config.repair_boot || config.is_xp_i386 {
        crate::core::pca_preflight::BootPreflight {
            pca_compat_package: None,
            uefiseven_source: None,
            secure_boot_disable_required: false,
        }
    } else {
        // Auto mode can become UEFI after target preparation, so preflight all
        // modes except explicit Legacy before the first target-disk mutation.
        let staged_pca_compat = match crate::core::pca_preflight::staged_config(
            &config,
            std::path::Path::new(&data_dir),
        ) {
            Ok(staged) => staged,
            Err(error) => {
                fail_prewrite!(error);
            }
        };
        match crate::core::pca_preflight::verify_before_disk_write(
            &image_path,
            config.volume_index,
            config.is_gho,
            config.is_xp,
            config.boot_mode != 2,
            config.boot_pca_mode,
            staged_pca_compat.as_ref(),
            std::path::Path::new(&data_dir),
        ) {
            Ok(package) => package,
            Err(error) => {
                fail_prewrite!(error);
            }
        }
    };
    let pca_compat_package = boot_preflight.pca_compat_package;
    let uefiseven_source = boot_preflight.uefiseven_source;
    let secure_boot_disable_required = boot_preflight.secure_boot_disable_required;

    // Before formatting, check only deterministic tree safety. Package signature and target
    // compatibility are deliberately left to Microsoft's actual DISM import result; duplicating
    // that policy here caused valid Wi-Fi/network/VMware packages to be rejected in WinPE.
    if config.should_import_drivers() {
        let driver_path = std::path::Path::new(&data_dir).join("drivers");
        if !driver_path.is_dir() {
            fail_prewrite!(tr!("驱动路径不存在: {}", driver_path.display()));
        }
        if !preserved_driver_infs.is_empty() {
            log::info!(
                "驱动目录结构预检完成: total={}；签名与兼容性以微软 DISM 实际导入结果为准",
                preserved_driver_infs.len()
            );
        } else {
            let error = anyhow::anyhow!("认证驱动清单中没有 INF 文件");
            log::error!("驱动目录写盘前结构预检失败，目标分区尚未修改: {error}");
            fail_prewrite!(tr!("驱动包预检失败: {}", error));
        }
    }

    let exact_image_spans = if config.is_xp_i386 {
        Vec::new()
    } else {
        match authenticated_task.install_image_span_paths() {
            Ok(paths) => paths,
            Err(error) => {
                fail_prewrite!(tr!("无法取得认证镜像分卷清单，已停止释放: {}", error));
            }
        }
    };
    if let Err(error) = crate::core::custom_install::validate_dual_boot_target(
        &config.custom_install_plan,
        expected_target,
        authenticated_task.data_volume_identity(),
    ) {
        fail_prewrite!(tr!("预创建的双系统目标已变化，尚未写入目标: {}", error));
    }
    let full_disk_preflight = match authenticated_task.full_disk_execution_targets() {
        Ok(targets) => match crate::core::custom_install::preflight_full_disk_install(
            &config.custom_install_plan,
            targets,
            authenticated_task.data_volume_identity(),
            expected_target,
        ) {
            Ok(value) => value,
            Err(error) => {
                fail_prewrite!(tr!("全盘重装布局预检失败，尚未清空任何硬盘: {}", error));
            }
        },
        Err(error) => {
            fail_prewrite!(tr!("读取全盘重装随机定位标志失败: {}", error));
        }
    };
    if config.is_xp_i386 && locked_xp_source.is_none() {
        fail_prewrite!(tr!("XP/2003 安装源未建立不可变目录清单，已停止复制"));
    }

    // Historical compatibility gate: arbitrary partition scripts are no longer executable.
    if config.run_diskpart_scripts {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("正在检查旧分区脚本兼容性...")));
        let scripts_dir = std::path::Path::new(&data_dir).join("diskpart");
        log::info!("[PE安装] 检查已停用的旧分区脚本: {}", scripts_dir.display());
        match lr_core::diskpart::run_scripts_in_dir(&scripts_dir) {
            Ok(out) => log::info!("[PE安装] 旧分区脚本兼容检查完成:\n{}", out),
            Err(e) => {
                log::error!("[PE安装] 旧分区脚本已停用: {}", e);
                fail_prewrite!(tr!("旧分区脚本已停用: {}", e));
            }
        }
    }

    if let Err(error) = DiskManager::validate_install_target_dependencies(
        &target_partition,
        expected_target,
        std::path::Path::new(&image_path),
    ) {
        fail_prewrite!(tr!(
            "安装来源或目标分区安全检查失败，尚未写入目标: {}",
            error
        ));
    }

    if let Err(error) = authenticated_task.verify_unchanged() {
        fail_prewrite!(tr!("首次写入前安装授权或输入已变化: {}", error));
    }
    let personal_file_plan = if config.preserve_personal_files {
        let target_root = std::path::PathBuf::from(format!(
            "{}\\",
            target_partition.trim().trim_end_matches(['\\', '/'])
        ));
        match lr_core::personal_files::plan_personal_file_preservation(
            &target_root,
            &config.session_id,
        ) {
            Ok(plan) => {
                log::info!(
                    "[PE PERSONAL FILES] preflight complete: directories={} files={} bytes={} destination={}",
                    plan.directories.len(),
                    plan.files,
                    plan.bytes,
                    plan.preserved_root.display()
                );
                Some(plan)
            }
            Err(error) => {
                fail_prewrite!(tr!(
                    "保留个人文件预检失败，尚未删除、格式化或覆盖原系统: {}",
                    error
                ));
            }
        }
    } else {
        None
    };
    #[cfg(feature = "ci-automation")]
    if let Err(error) = crate::inject_ci_failure_before_target_write() {
        log::error!("[CI AUTOMATION] {error:#}");
        fail_prewrite!(tr!("安装故障注入: {}", error));
    }
    // Install may format the persistent PE carrier. Remove only the LRPE4 objects authenticated
    // by the still-live X: capsule after every public input/preflight has succeeded and immediately
    // before arming the first target-side write.
    if let Err(error) = crate::cleanup_persistent_pe_boot_payload(authenticated_task.guard()) {
        fail_prewrite!(tr!("清理本次 PE 启动项失败，尚未写入目标分区: {}", error));
    }
    // Ordinary/dual installs keep the selected volume, so remove their exact marker before
    // writing it. Full-disk mode is different: the checked topology transaction immediately
    // removes the old partition. Deleting locator files one-by-one first has no safety benefit
    // and can leave a half-unpublished task if one deletion fails; its release method therefore
    // only closes the verified handles.
    if full_disk_preflight.is_none() {
        if let Err(error) = authenticated_task.release_install_target_marker() {
            fail_prewrite!(tr!("释放本次安装目标标记失败，尚未写入目标分区: {}", error));
        }
    }

    if let Some(prepared) = full_disk_preflight {
        let released = match authenticated_task.release_full_disk_markers() {
            Ok(targets) => targets,
            Err(error) => {
                fail_prewrite!(tr!("释放全盘重装随机定位标志失败，尚未清空硬盘: {}", error));
            }
        };
        log::warn!(
            "[PE INSTALL] irreversible boundary entered: full-disk repartition started; old-system rollback is disabled"
        );
        match crate::core::custom_install::execute_full_disk_install(prepared, &released) {
            Ok(target) => {
                target_partition = target.partition;
                expected_target = target.identity;
                full_disk_staging_cleanup = target.staging_cleanup;
            }
            Err(error) => {
                log::error!(
                    "[FULL DISK] repartition failed after the irreversible boundary: {error:#}"
                );
                // The full-disk transaction has already deleted the old system layout, but the
                // ordinary destructive-stage finalizer is armed only after a new Windows target
                // exists. Persist this exact failure to the authenticated staging volume before
                // automation powers the disposable VM off; otherwise the only useful VDS error
                // remains on X: and is lost with the PE RAM disk.
                let terminal_log = crate::InstallLogTerminalFinalizer::armed(
                    &target_partition,
                    &public_data_root,
                    &public_data_dir,
                    &config.session_id,
                );
                drop(terminal_log);
                let _ = tx.send(WorkerMessage::Failed(tr!("全盘重装分区失败: {}", error)));
                return;
            }
        }
    }

    // Arm before formatting so a destructive-stage failure keeps the PE tail on the validated
    // data partition instead of losing it with the RAM disk.
    let mut terminal_log = crate::InstallLogTerminalFinalizer::armed(
        &target_partition,
        &public_data_root,
        &public_data_dir,
        &config.session_id,
    );

    // Step 1: 格式化分区
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::FormatPartition));
    let format_target = config.format_partition
        && matches!(
            &config.custom_install_plan,
            lr_core::custom_install::CustomInstallPlan::ReinstallPartition
        );
    let personal_files_prepared = if let Some(plan) = personal_file_plan.as_ref() {
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "正在保留个人文件并快速删除旧系统..."
        )));
        if let Err(error) =
            DiskManager::verify_partition_volume_identity(&target_partition, expected_target)
        {
            fail_prewrite!(tr!(
                "保留个人文件前目标分区物理身份已变化，安装已停止: {}",
                error
            ));
        }
        if let Err(error) = cleanup_target_private_pe_residue(&target_partition) {
            fail_prewrite!(tr!("清理目标分区上次任务遗留的 PE 文件失败: {}", error));
        }
        match lr_core::personal_files::execute_personal_file_preservation(plan, || {
            log::warn!(
                "[PE INSTALL] irreversible boundary entered: personal files preserved and old-system deletion started; old-system rollback is disabled"
            );
        }) {
            Ok(report) => {
                log::info!(
                    "[PE PERSONAL FILES] complete: destination={} directories={} files={} bytes={} deleted_roots={} deleted_entries={} deleted_desktop_shortcuts={} unresolved_desktop_shortcuts={}",
                    report.preserved_root.display(),
                    report.preserved_directories,
                    report.preserved_files,
                    report.preserved_bytes,
                    report.deleted_roots,
                    report.deleted_entries,
                    report.deleted_desktop_shortcuts,
                    report.unresolved_desktop_shortcuts
                );
                true
            }
            Err(error) if error.stage == lr_core::personal_files::PreservationStage::Reversible => {
                fail_prewrite!(tr!(
                    "保留个人文件失败，已恢复所有已移动目录且尚未删除旧系统: {}",
                    error
                ));
            }
            Err(error) => {
                log::error!(
                    "[PE PERSONAL FILES] partial state stage={:?} destination={}: {:#}",
                    error.stage,
                    error.preserved_root.display(),
                    error
                );
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "保留个人文件任务已进入部分完成状态，禁止自动回退；保留目录位于 {}。错误: {}",
                    error.preserved_root.display(),
                    error
                )));
                return;
            }
        }
    } else {
        false
    };
    if format_target {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("正在格式化目标分区...")));
        // Point of no return. A failed format can already have destroyed file-system metadata, so
        // no later error path is allowed to restore the old OS, old boot state, or pre-write
        // session transaction. From here on we preserve diagnostics and report the actual failure.
        log::warn!(
            "[PE INSTALL] irreversible boundary entered: target format started; old-system rollback is disabled"
        );

        // 使用卷标参数（如果有配置的话）
        let volume_label = if config.volume_label.is_empty() {
            None
        } else {
            Some(config.volume_label.as_str())
        };

        match DiskManager::format_partition_with_label(
            &target_partition,
            expected_target,
            volume_label,
        ) {
            Ok(_) => log::info!("分区格式化成功"),
            Err(e) => {
                log::error!("[PE安装] 格式化分区失败: {}", e);
                let _ = tx.send(WorkerMessage::Failed(tr!("格式化分区失败: {}", e)));
                return;
            }
        }
        #[cfg(feature = "ci-automation")]
        if let Err(error) = crate::inject_ci_failure_after_target_format() {
            log::error!("[CI AUTOMATION] {error:#}");
            let _ = tx.send(WorkerMessage::Failed(tr!("安装故障注入: {}", error)));
            return;
        }
    } else {
        log::info!("[PE安装] 用户已关闭格式化目标分区，跳过格式化");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 2: 释放镜像
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::ApplyImage));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在释放系统镜像...")));
    if config.is_xp_i386 {
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "正在准备 XP/2003 文本模式安装..."
        )));
        let locked = locked_xp_source
            .as_ref()
            .expect("XP source lock was checked before the irreversible boundary");
        if !format_target {
            if let Err(error) =
                DiskManager::verify_partition_volume_identity(&target_partition, expected_target)
            {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "首次写入前目标分区物理身份已变化，安装已停止: {}",
                    error
                )));
                return;
            }
            log::warn!(
                "[PE INSTALL] irreversible boundary entered: first XP source write is about to start; old-system rollback is disabled"
            );
            if let Err(error) = cleanup_target_private_pe_residue(&target_partition) {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "清理目标分区上次任务遗留的 PE 文件失败: {}",
                    error
                )));
                return;
            }
        }
        match lr_core::xp_i386::install_from_i386_locked(
            locked,
            &target_partition,
            &crate::utils::path::get_bin_dir(),
            xp_custom_sif.as_deref(),
        ) {
            Ok(log_output) => log::info!("[PE安装/XP文本模式] {log_output}"),
            Err(error) => {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "准备 XP/2003 文本模式安装失败: {}",
                    error
                )));
                return;
            }
        }
        terminal_log.mark_target_system_available();
        let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Cleanup));
        let mut cleanup_warning = None;
        let auto_staging = match authenticated_task.into_install_cleanup_authorization() {
            Ok(authorization) => authorization,
            Err(error) => {
                log::warn!(
                    "[PE INSTALL/XP TEXTMODE] installation succeeded but session cleanup could not finish: {error:#}"
                );
                cleanup_warning = Some(tr!(
                    "XP/2003 系统已安装完成，但本次会话清理未完成；请手动重启后处理残留临时文件: {}",
                    error
                ));
                None
            }
        };
        if let Some(authorization) = full_disk_staging_cleanup.take() {
            match crate::core::custom_install::cleanup_full_disk_staging(&authorization) {
                Ok(_) => {}
                Err(error) => {
                    log::warn!(
                        "[PE INSTALL/XP TEXTMODE] installation succeeded but full-disk staging cleanup failed: {error:#}"
                    );
                    cleanup_warning = Some(tr!(
                        "XP/2003 系统已安装完成，但临时分区未能清理；请手动重启后处理: {}",
                        error
                    ));
                }
            }
        } else if let Some(authorization) = auto_staging {
            match DiskManager::cleanup_authenticated_auto_staging(&authorization) {
                Ok(_) => {}
                Err(error) => {
                    log::warn!(
                        "[PE INSTALL/XP TEXTMODE] installation succeeded but authenticated staging cleanup failed: {error:#}"
                    );
                    cleanup_warning = Some(tr!(
                        "XP/2003 系统已安装完成，但临时分区未能清理；请手动重启后处理: {}",
                        error
                    ));
                }
            }
        }
        let _ = tx.send(WorkerMessage::SetProgress(100));
        let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Complete));
        automation_failure_shutdown.disarm();
        if let Some(warning) = cleanup_warning {
            let _ = tx.send(WorkerMessage::CompletedWithWarning(warning));
            terminal_log.finish_success(false);
            if should_reboot_after_completion_warning(
                config.auto_reboot,
                config.automation_shutdown_on_terminal,
            ) {
                log::info!("XP/2003 文本模式安装带警告完成，即将请求重启");
                std::thread::sleep(std::time::Duration::from_secs(3));
                reboot_pe();
            } else {
                log::warn!("XP/2003 文本模式安装带警告完成，等待用户查看并手动重启");
            }
            return;
        }
        let _ = tx.send(WorkerMessage::Completed);
        if config.auto_reboot || config.automation_shutdown_on_terminal {
            log::info!("XP/2003 文本模式安装完成，即将请求重启");
            terminal_log.finish_success(true);
            std::thread::sleep(std::time::Duration::from_secs(3));
            reboot_pe();
        } else {
            log::info!("XP/2003 文本模式安装已完成，按配置等待用户手动重启");
            terminal_log.finish_success(true);
        }
        return;
    }

    let apply_dir = format!("{}\\", target_partition);

    log::info!(
        "[PE安装] 开始释放镜像: 文件={} 卷索引={} is_gho={} -> 目标={}",
        image_path,
        config.volume_index,
        config.is_gho,
        apply_dir
    );

    // 创建进度通道
    let (progress_tx, progress_rx) = channel::<DismProgress>();
    let tx_clone = tx.clone();

    // 启动进度监控线程
    let progress_handle = thread::spawn(move || {
        while let Ok(progress) = progress_rx.recv() {
            let _ = tx_clone.send(WorkerMessage::SetProgress(progress.percentage));
        }
    });

    if !format_target {
        if let Err(error) =
            DiskManager::verify_partition_volume_identity(&target_partition, expected_target)
        {
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "首次写入前目标分区物理身份已变化，安装已停止: {}",
                error
            )));
            return;
        }
        // Applying without formatting still overwrites the old installation. Disable rollback
        // immediately before the image engine receives the target path.
        if !personal_files_prepared {
            log::warn!(
                "[PE INSTALL] irreversible boundary entered: first image write is about to start; old-system rollback is disabled"
            );
            if let Err(error) = cleanup_target_private_pe_residue(&target_partition) {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "清理目标分区上次任务遗留的 PE 文件失败: {}",
                    error
                )));
                return;
            }
        }
    }
    let apply_result = if config.is_gho {
        // GHO镜像使用Ghost
        let ghost = Ghost::new();
        let partitions = DiskManager::get_partitions().unwrap_or_default();
        ghost.restore_image_to_letter(
            &image_path,
            &target_partition,
            &partitions,
            Some(progress_tx),
        )
    } else {
        // WIM/ESD使用DISM
        let dism = Dism::new();
        dism.apply_image_with_exact_swm_resources(
            &image_path,
            &exact_image_spans,
            &apply_dir,
            config.volume_index,
            Some(progress_tx),
        )
    };

    // 等待进度监控线程结束
    let _ = progress_handle.join();

    if let Err(e) = apply_result {
        log::error!("[PE安装] 释放镜像失败: {}", e);
        let _ = tx.send(WorkerMessage::Failed(tr!("释放镜像失败: {}", e)));
        return;
    }
    log::info!("[PE安装] 释放镜像完成");
    terminal_log.mark_target_system_available();
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let mut completion_warnings = Vec::new();
    let account_inspection =
        crate::core::account_fix::inspect_offline_image_accounts(&target_partition, config.is_gho);
    log::info!(
        "[ACCOUNT MODE] mode={:?} image_state={:?} local_accounts={} ordinary_accounts={} detail={}",
        account_inspection.mode,
        account_inspection.image_state,
        account_inspection.local_account_count,
        account_inspection.ordinary_local_account_count,
        account_inspection.diagnostic
    );
    if !account_inspection.allows_new_install_unattended() {
        // A captured installation owns its existing SAM and login state. Never try to make an
        // answer file "work" by blanking passwords, enabling accounts, or replacing Winlogon.
        // Indeterminate evidence follows the same non-mutating path while the core image/boot
        // installation continues normally.
        let requested_unattended_features = suppress_new_install_only_options(&mut config);
        selected_preinstalled_software.clear();
        private_wifi_profile = None;

        if requested_unattended_features {
            completion_warnings.push(tr!(
                "检测到该镜像包含已有账户或不属于可确认的新装状态，已保留原账户与密码，并跳过无人值守、账户修改、预装软件和首次登录任务。"
            ));
        }
    }

    // Step 3: 导入驱动
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::ImportDrivers));

    // 根据 driver_action_mode 决定是否导入驱动
    // 0 = 无, 1 = 仅保存（不导入）, 2 = 自动导入
    let driver_path = format!("{}\\drivers", data_dir);
    let driver_path_exists = std::path::Path::new(&driver_path).exists();
    if config.should_import_drivers() && driver_path_exists {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("正在导入驱动...")));

        // 创建进度通道
        let (driver_progress_tx, driver_progress_rx) = channel::<DismProgress>();
        let tx_driver = tx.clone();

        // 启动进度监控线程
        let driver_progress_handle = thread::spawn(move || {
            while let Ok(progress) = driver_progress_rx.recv() {
                let _ = tx_driver.send(WorkerMessage::SetProgressStatus {
                    progress: progress.percentage,
                    status: tr!("导入驱动: {}", progress.status),
                });
            }
        });

        let dism = Dism::new();
        let import_result = dism.add_preserved_driver_inf_files_offline_with_progress(
            &apply_dir,
            &preserved_driver_infs,
            Some(driver_progress_tx),
        );

        // 等待进度监控线程结束
        let _ = driver_progress_handle.join();
        let optional_failures = match import_result {
            Ok(failures) => failures,
            Err(error) => {
                log::error!("导入驱动失败，安装停止: {}", error);
                let _ = tx.send(WorkerMessage::Failed(tr!("离线驱动导入失败: {}", error)));
                return;
            }
        };
        let failure_summary =
            lr_core::bounded_failure_summary::summarize_failures(&optional_failures, 3);
        if !failure_summary.is_empty() {
            log::warn!("部分非启动存储驱动未能由标准 DISM 导入: {failure_summary}");
        }
        if let Err(error) = lr_core::driver::verify_offline_storage_driver_requirements(
            Path::new(&apply_dir),
            Path::new(&driver_path),
        ) {
            log::error!("启动存储驱动导入后验证失败，安装停止: {}", error);
            let _ = tx.send(WorkerMessage::Failed(tr!("离线驱动导入失败: {}", error)));
            return;
        } else {
            log::info!(
                "驱动导入完成，启动存储驱动覆盖验证通过；跳过可选包 {} 个",
                optional_failures.len()
            );
        }

        // Optional CABs are selected from the same exact authenticated artifact set. Never scan
        // the public driver directory again after authentication.
        let (cab_progress_tx, cab_progress_rx) = channel::<DismProgress>();
        let tx_cab = tx.clone();
        let cab_progress_handle = thread::spawn(move || {
            while let Ok(progress) = cab_progress_rx.recv() {
                let _ = tx_cab.send(WorkerMessage::SetProgressStatus {
                    progress: progress.percentage,
                    status: tr!("安装CAB: {}", progress.status),
                });
            }
        });
        let dism = Dism::new();
        let cab_result = dism.add_optional_package_paths_offline(
            &apply_dir,
            &preserved_driver_cabs,
            Some(cab_progress_tx),
        );
        let _ = cab_progress_handle.join();
        match cab_result {
            Ok((success, failed)) if success > 0 || failed > 0 => {
                if failed > 0 {
                    log::warn!(
                        "驱动目录中的可选 CAB 部分失败，安装继续: {} 成功, {} 失败",
                        success,
                        failed
                    );
                } else {
                    log::info!("驱动目录中的CAB安装完成: {} 成功", success);
                }
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!("驱动目录中的可选 CAB 扫描或安装不可用，已跳过并继续: {error}");
            }
        }
    } else if config.should_import_drivers() && !driver_path_exists {
        log::error!("请求自动导入驱动，但驱动目录不存在: {}", driver_path);
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "驱动路径不存在: {}",
            driver_path
        )));
        return;
    } else if config.has_driver_data() {
        if !driver_path_exists {
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "请求保留驱动，但暂存驱动目录不存在: {}",
                driver_path
            )));
            return;
        }
        let destination =
            match crate::save_only_driver_destination(&target_partition, &config.session_id) {
                Ok(path) => path,
                Err(error) => {
                    let _ = tx.send(WorkerMessage::Failed(tr!("保留驱动失败: {}", error)));
                    return;
                }
            };
        match lr_core::windows_file_copy::copy_tree_verified(
            std::path::Path::new(&driver_path),
            &destination,
        ) {
            Ok(0) => {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "请求保留驱动，但暂存目录中没有可保存的文件"
                )));
                return;
            }
            Ok(files) => {
                let _ = tx.send(WorkerMessage::SetStatus(tr!(
                    "驱动已保存（{} 个文件）",
                    files
                )));
                log::info!(
                    "SaveOnly driver tree copied and verified: files={}, destination={}",
                    files,
                    destination.display()
                );
            }
            Err(error) => {
                log::error!("SaveOnly driver preservation failed: {error:#}");
                let _ = tx.send(WorkerMessage::Failed(tr!("保留驱动失败: {}", error)));
                return;
            }
        }
    } else {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("跳过驱动导入")));
        log::info!("驱动操作模式为无，跳过驱动导入");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 4: 安装CAB更新包
    let _ = tx.send(WorkerMessage::SetInstallStep(
        InstallStep::InstallCabPackages,
    ));

    if config.install_cab_packages {
        if !update_package_artifacts.is_empty() {
            let _ = tx.send(WorkerMessage::SetStatus(tr!("正在安装更新包...")));

            // 创建进度通道
            let (cab_progress_tx, cab_progress_rx) = channel::<DismProgress>();
            let tx_cab = tx.clone();

            // 启动进度监控线程
            let cab_progress_handle = thread::spawn(move || {
                while let Ok(progress) = cab_progress_rx.recv() {
                    let _ = tx_cab.send(WorkerMessage::SetProgressStatus {
                        progress: progress.percentage,
                        status: tr!("安装更新: {}", progress.status),
                    });
                }
            });

            let dism = Dism::new();
            let cab_result = dism.add_optional_package_paths_offline(
                &apply_dir,
                &update_package_artifacts,
                Some(cab_progress_tx),
            );
            let _ = cab_progress_handle.join();
            match cab_result {
                Ok((success, failed)) if success > 0 || failed > 0 => {
                    if failed > 0 {
                        log::warn!(
                            "可选 CAB 更新包部分失败，安装继续: {} 成功, {} 失败",
                            success,
                            failed
                        );
                    } else {
                        log::info!("CAB更新包安装完成: {} 成功", success);
                    }
                    let _ = tx.send(WorkerMessage::SetStatus(tr!(
                        "更新包安装完成: {} 成功, {} 失败",
                        success,
                        failed
                    )));
                }
                Ok(_) => {
                    log::warn!("认证更新清单中没有可安装的 CAB 包，安装继续");
                }
                Err(error) => {
                    log::warn!("可选 CAB 更新包扫描或安装不可用，已跳过并继续: {error}");
                }
            }
        } else {
            log::warn!("未提供认证 CAB 更新包，已跳过并继续");
        }
    } else {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("跳过更新包安装")));
        log::info!("未启用CAB更新包安装");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    if let Some(package) = pca_compat_package.as_ref() {
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "正在升级 PCA2023 引导文件..."
        )));
        log::info!(
            "[PE安装] 为 Windows build {} / architecture {} 注入 PCA2023 BootEx",
            package.target().build,
            package.target().architecture
        );
        if let Err(error) = package.inject_into_offline_windows(std::path::Path::new(&apply_dir)) {
            log::error!("[PE安装] PCA2023 兼容包注入失败: {error}");
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "升级 PCA2023 引导文件失败：{}",
                error
            )));
            return;
        }
    }

    // Step 5: 修复引导
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::RepairBoot));
    if config.repair_boot {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("正在修复引导...")));

        let boot_manager = BootManager::new();
        let use_uefi =
            match DiskManager::resolve_install_uefi_mode(config.boot_mode, &target_partition) {
                Ok(value) => value,
                Err(error) => {
                    log::error!("[PE安装] 无法可靠确定引导模式: {error}");
                    let _ = tx.send(WorkerMessage::Failed(tr!(
                        "无法可靠确定引导模式，已停止安装：{}",
                        error
                    )));
                    return;
                }
            };

        // NT5 只能来自已经验证并写入交接配置的安装意图。不能再通过目录缺失
        // 猜测 XP，否则损坏或精简的现代镜像会被错误写入 NTLDR 引导。
        let is_xp = config.is_xp || config.is_xp_i386;
        let boot_result = if is_xp {
            if use_uefi {
                log::info!("[PE安装] 识别为 XP/2003 + UEFI，写入 XP UEFI/GPT 引导");
                boot_manager.write_xp_uefi_gpt_boot(&target_partition)
            } else {
                log::info!("[PE安装] 识别为 XP/2003(Legacy)，写入 XP 引导(ntldr/boot.ini)");
                boot_manager.write_xp_boot(&target_partition)
            }
        } else {
            boot_manager.repair_boot_advanced(&target_partition, use_uefi, config.boot_pca_mode)
        };
        if let Err(e) = boot_result {
            log::error!("[PE安装] 修复引导失败: {e}");
            let _ = tx.send(WorkerMessage::Failed(tr!("修复引导失败: {}", e)));
            return;
        }

        if use_uefi && uefiseven_source.is_some() {
            log::info!("[PE安装] 部署 Win7 x64 UEFI 兼容加载器 UefiSeven");
            if let Err(error) = crate::ui::advanced_options::apply_uefiseven_patch(
                uefiseven_source.as_deref().expect("checked above"),
                &target_partition,
            ) {
                log::error!("[PE安装] UefiSeven 部署失败: {error}");
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "部署 Windows 7 UEFI 兼容加载器失败: {}",
                    error
                )));
                return;
            }
            if secure_boot_disable_required {
                completion_warnings.push(tr!("Windows 7 UEFI 已安装完成，但当前 Secure Boot（安全启动）仍处于开启状态。请先进入 BIOS/UEFI 关闭 Secure Boot，再启动新系统。程序不会自动重启。"));
            }
        }
    } else {
        log::info!("[PE安装] 用户已关闭添加引导，跳过引导模式探测和引导写入");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 6: 应用高级选项
    let _ = tx.send(WorkerMessage::SetInstallStep(
        InstallStep::ApplyAdvancedOptions,
    ));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在应用高级选项...")));

    if config.has_requested_advanced_options() {
        if let Err(e) = apply_advanced_options(&target_partition, &config) {
            log::warn!("部分高级可选项未能应用，安装继续: {e:#}");
            let _ = tx.send(WorkerMessage::SetStatus(tr!(
                "部分高级选项未能应用，正在继续完成安装"
            )));
        }
    } else {
        log::info!("未启用高级选项，跳过离线注册表加载");
    }
    if let Some(profile) = private_wifi_profile.as_deref() {
        match lr_core::first_logon::stage_wifi_profile(&target_partition, profile) {
            Ok(_) => log::info!(
                "[ADVANCED WIFI] status=staged source=authenticated_private_boot_payload"
            ),
            Err(error) => {
                log::warn!(
                    "[ADVANCED WIFI] status=warning detail=failed_to_stage_authenticated_profile: {error:#}"
                );
                completion_warnings.push(tr!("系统已安装，但当前 Wi-Fi 配置未能迁移: {}", error));
            }
        }
    }
    // 仅消费 typed task 中 exact、仍由句柄锁定的用户驱动清单。
    if let Err(error) = crate::ui::advanced_options::inject_user_drivers_from_authenticated_paths(
        &target_partition,
        &user_driver_artifacts,
    ) {
        log::warn!("可选用户驱动恢复不可用，安装继续: {error:#}");
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "部分用户驱动未能恢复，正在继续完成安装"
        )));
    }
    if let Err(error) = stage_authenticated_preinstalled_software(
        &target_partition,
        &selected_preinstalled_software,
        &preinstalled_software_artifacts,
    ) {
        log::error!("预装软件暂存失败: {error:#}");
        let _ = tx.send(WorkerMessage::Failed(tr!("预装软件暂存失败: {}", error)));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 7: 生成无人值守配置
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::GenerateUnattend));

    if config.unattended {
        if !config.custom_unattend_file.is_empty() {
            if config.disable_windows_defender {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "移除 Windows 安全中心需要使用 LetRecovery 内置无人值守配置，不能与自定义应答文件同时使用"
                )));
                return;
            }
            if config.disable_reserved_storage {
                log::warn!(
                    "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=custom_unattend_not_modified"
                );
            }
            if config.remove_uwp_apps {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "移除预装应用需要使用 LetRecovery 内置无人值守配置，不能与自定义应答文件同时使用"
                )));
                return;
            }
            // 用户提供了自定义无人值守文件：直接复制到目标系统（不再内置生成）
            let _ = tx.send(WorkerMessage::SetStatus(tr!(
                "正在应用自定义无人值守配置..."
            )));
            let src = match ConfigFileManager::resolve_staged_file(
                &data_dir,
                &config.custom_unattend_file,
            ) {
                Ok(path) => path,
                Err(error) => {
                    let _ = tx.send(WorkerMessage::Failed(tr!(
                        "自定义无人值守文件名无效: {}",
                        error
                    )));
                    return;
                }
            };
            match apply_custom_unattend(&target_partition, &src.to_string_lossy()) {
                Ok(_) => log::info!("[UNATTEND] 已应用自定义无人值守文件: {}", src.display()),
                Err(e) => {
                    log::error!("应用自定义无人值守文件失败: {}", e);
                    let _ = tx.send(WorkerMessage::Failed(tr!(
                        "应用自定义无人值守配置失败: {}",
                        e
                    )));
                    return;
                }
            }
        } else {
            let _ = tx.send(WorkerMessage::SetStatus(tr!("正在生成无人值守配置...")));
            if let Err(e) = generate_unattend_xml(&target_partition, &config) {
                log::error!("生成无人值守配置失败: {}", e);
                let _ = tx.send(WorkerMessage::Failed(tr!("生成无人值守配置失败: {}", e)));
                return;
            }
        }
    } else {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("跳过无人值守配置")));
    }

    if !config.unattended && config.disable_windows_defender {
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "移除 Windows 安全中心需要启用无人值守安装"
        )));
        return;
    }
    if !config.unattended && config.disable_reserved_storage {
        log::warn!(
            "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=unattended_install_disabled"
        );
    }
    if !config.unattended && config.remove_uwp_apps {
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "移除预装应用需要启用无人值守安装"
        )));
        return;
    }

    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 8: 清理临时文件
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Cleanup));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理临时文件...")));

    // Delete only the two control files represented by the live authenticated kernel handles.
    // Auto-staging partition deletion remains fail-closed until the move-only canonical staging
    // authorization can be transferred to the checked PhysicalDrive transaction.
    let mut cleanup_verified = true;
    let auto_staging = match authenticated_task.into_install_cleanup_authorization() {
        Ok(authorization) => authorization,
        Err(error) => {
            log::warn!(
                "[PE INSTALL] installation succeeded but session cleanup could not finish: {error:#}"
            );
            completion_warnings.push(tr!(
                "系统已安装完成，但本次会话清理未完成；请手动重启后处理残留临时文件: {}",
                error
            ));
            cleanup_verified = false;
            None
        }
    };
    if let Some(authorization) = full_disk_staging_cleanup.take() {
        match crate::core::custom_install::cleanup_full_disk_staging(&authorization) {
            Ok(_) => {}
            Err(error) => {
                log::warn!(
                    "[PE INSTALL] installation succeeded but full-disk staging cleanup failed: {error:#}"
                );
                completion_warnings.push(tr!(
                    "系统已安装完成，但临时分区未能清理；请手动重启后处理: {}",
                    error
                ));
                cleanup_verified = false;
            }
        }
    } else if let Some(authorization) = auto_staging {
        match DiskManager::cleanup_authenticated_auto_staging(&authorization) {
            Ok(_) => {}
            Err(error) => {
                log::warn!(
                    "[PE INSTALL] installation succeeded but authenticated staging cleanup failed: {error:#}"
                );
                completion_warnings.push(tr!(
                    "系统已安装完成，但临时分区未能清理；请手动重启后处理: {}",
                    error
                ));
                cleanup_verified = false;
            }
        }
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // 完成
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Complete));
    if !selected_preinstalled_software.is_empty() {
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "Windows 离线部署已完成，即将重启。预装软件将在首次登录阶段继续安装。"
        )));
    }
    automation_failure_shutdown.disarm();
    let completion_warning = completion_warnings.join("\n");
    if !completion_warning.is_empty() {
        let _ = tx.send(WorkerMessage::CompletedWithWarning(completion_warning));
        terminal_log.finish_success(cleanup_verified);
        if should_reboot_after_completion_warning(
            config.auto_reboot,
            config.automation_shutdown_on_terminal,
        ) {
            log::warn!(
                "post-install warning recorded; system is bootable and automatic restart continues"
            );
            std::thread::sleep(std::time::Duration::from_secs(3));
            reboot_pe();
        } else {
            log::warn!("post-install warning recorded; waiting for the requested manual restart");
        }
        return;
    }

    let _ = tx.send(WorkerMessage::Completed);

    log::info!("========== PE安装流程完成 ==========");

    if config.auto_reboot || config.automation_shutdown_on_terminal {
        log::info!("即将重启...");
        terminal_log.finish_success(cleanup_verified);
        std::thread::sleep(std::time::Duration::from_secs(3));
        reboot_pe();
    } else {
        log::info!("安装已完成，按配置等待用户手动重启");
        terminal_log.finish_success(cleanup_verified);
    }
}

/// 生成无人值守XML
///
/// 包含完整的无人值守配置，并根据目标系统版本自动适配：
/// - Windows 10/11: 完整的 OOBE 跳过设置
/// - Windows 7/8/8.1: 兼容的简化配置
///
/// 同时自动检测目标系统架构（x86/amd64/arm64）
///
/// 配置内容包括：
/// - windowsPE pass: 基本设置
/// - specialize pass: 部署脚本执行
/// - oobeSystem pass: OOBE设置、用户账户、首次登录命令
///
/// 应用用户自定义的无人值守文件：复制到目标系统的 Panther 与 Sysprep 目录
pub(crate) fn apply_custom_unattend(target_partition: &str, src: &str) -> anyhow::Result<()> {
    let content = std::fs::read(src)
        .map_err(|e| anyhow::anyhow!("读取自定义无人值守文件失败 {}: {}", src, e))?;

    let panther_dir = format!("{}\\Windows\\Panther", target_partition);
    std::fs::create_dir_all(&panther_dir)?;
    std::fs::write(format!("{}\\unattend.xml", panther_dir), &content)?;

    let sysprep_dir = format!("{}\\Windows\\System32\\Sysprep", target_partition);
    if std::path::Path::new(&sysprep_dir).exists() {
        let _ = std::fs::write(format!("{}\\unattend.xml", sysprep_dir), &content);
    }
    Ok(())
}

pub(crate) fn generate_unattend_xml(
    target_partition: &str,
    config: &crate::core::config::InstallConfig,
) -> anyhow::Result<()> {
    use crate::core::system_utils::{get_file_version, get_offline_system_architecture};
    use std::path::Path;

    let selected_preinstalled_software = config.selected_preinstalled_software()?;
    let temporary_oobe_account = config
        .builtin_administrator
        .enabled
        .then(|| lr_core::unattend_account::temporary_oobe_account_name(&config.session_id))
        .transpose()
        .map_err(|error| anyhow::anyhow!("无法生成临时 OOBE 账户: {error}"))?;
    lr_core::first_logon::stage_with_software_shutdown_and_personal_restore_and_builtin(
        target_partition,
        &selected_preinstalled_software,
        config.automation_shutdown_on_terminal,
        config
            .preserve_personal_files
            .then_some(config.session_id.as_str()),
        temporary_oobe_account.as_deref().map(|temporary_name| {
            lr_core::first_logon::BuiltinAdministratorTransitionAccounts {
                desired_name: config.builtin_administrator.account_name.as_str(),
                temporary_name,
                password: &config.builtin_administrator.password,
            }
        }),
    )?;
    // Windows Setup can leave a disabled `defaultuser0` account even for an ordinary
    // unattended local-account install. The first-logon finalizer always owns that bounded
    // cleanup, so its native NetAPI/Profile helper must be staged for every install rather
    // than only for personal-file restore or the built-in Administrator transition.
    lr_core::first_logon::stage_account_helper(target_partition)?;

    let builtin = lr_core::unattend_account::render_builtin_administrator_unattend(
        &config.builtin_administrator,
        2,
        temporary_oobe_account.as_deref().unwrap_or_default(),
    )
    .map_err(|error| anyhow::anyhow!("内置 Administrator 配置无效: {error}"))?;
    let (mut specialize_account_command, user_accounts, auto_logon) = if let Some(builtin) = builtin
    {
        log::info!(
            "[UNATTEND] 使用内置 RID-500 Administrator，账户名={}，首次自动登录=true（跳过 OOBE），密码=已设置",
            config.builtin_administrator.account_name,
        );
        (
            builtin.specialize_command,
            builtin.user_accounts,
            builtin.auto_logon,
        )
    } else {
        let raw_username = if config.custom_username.is_empty() {
            "User"
        } else {
            &config.custom_username
        };
        lr_core::unattend_account::validate_unattended_local_account_name(raw_username)
            .map_err(|error| anyhow::anyhow!("自定义用户名无效: {error}"))?;
        let username = escape_xml_text(raw_username);
        (
            String::new(),
            format!(
                r#"<UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password><Value></Value><PlainText>true</PlainText></Password>
                        <Description>Local User</Description>
                        <DisplayName>{username}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{username}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>"#
            ),
            format!(
                r#"<AutoLogon>
                <Password><Value></Value><PlainText>true</PlainText></Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{username}</Username>
            </AutoLogon>"#
            ),
        )
    };

    // 检测目标系统架构
    let arch = get_offline_system_architecture(Path::new(target_partition));
    let arch_str = arch.as_unattend_str();
    log::info!("[UNATTEND] 检测到目标系统架构: {}", arch_str);

    // 通过 ntdll.dll 文件版本检测目标系统版本
    // Windows 7: 6.1.x, Windows 8: 6.2.x, Windows 8.1: 6.3.x, Windows 10/11: 10.0.x
    let ntdll_path = Path::new(target_partition)
        .join("Windows")
        .join("System32")
        .join("ntdll.dll");
    let (is_win7, is_win8, is_win10_or_11, target_version) = match get_file_version(&ntdll_path) {
        Some((major, minor, build, _)) => {
            log::info!(
                "[UNATTEND] 检测到目标系统版本 (ntdll.dll): {}.{}.{}",
                major,
                minor,
                build
            );

            let is_win7 = major == 6 && minor == 1;
            let is_win8 = major == 6 && (minor == 2 || minor == 3);
            (
                is_win7,
                is_win8,
                major == 10 && minor == 0,
                Some((major, minor, build)),
            )
        }
        None => {
            log::warn!(
                "[UNATTEND] 无法读取 ntdll.dll 版本: {:?}, 默认使用 Win10/11 配置",
                ntdll_path
            );
            (false, false, false, None)
        }
    };

    if config.disable_windows_defender {
        if !is_win10_or_11 {
            anyhow::bail!("Windows Security UI removal requires a confirmed Windows 10/11 target");
        } else {
            let path = lr_core::sec_health_ui::stage_online_removal_script(target_partition)?;
            if !lr_core::sec_health_ui::online_script_is_staged(target_partition)? {
                anyhow::bail!("Windows Security UI removal script readback mismatch");
            }
            specialize_account_command
                .push_str(&lr_core::sec_health_ui::render_specialize_command(3)?);
            log::info!(
                "[ADVANCED_SEC_HEALTH_UI] phase=online_hook status=staged path={:?}",
                path
            );
        }
    }

    if config.disable_reserved_storage {
        match target_version {
            Some((major, minor, build))
                if lr_core::reserved_storage::is_supported_target_version(
                    major, minor, build,
                ) =>
            {
                match lr_core::reserved_storage::stage_online_disable_script(target_partition) {
                    Ok(path) => match lr_core::reserved_storage::render_specialize_command(4) {
                        Ok(command) => {
                            specialize_account_command.push_str(&command);
                            log::info!(
                                "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=staged build={} path={:?}",
                                build,
                                path
                            );
                        }
                        Err(error) => log::warn!(
                            "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=command_render_failed detail={:?}",
                            error.to_string()
                        ),
                    },
                    Err(error) => log::warn!(
                        "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=script_stage_failed detail={:?}",
                        error.to_string()
                    ),
                }
            }
            Some((major, minor, build)) => log::warn!(
                "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=unsupported_target_version version={}.{}.{} minimum_build=19041",
                major,
                minor,
                build
            ),
            None => log::warn!(
                "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=target_version_unconfirmed"
            ),
        }
    }

    if config.remove_uwp_apps {
        if !is_win10_or_11 {
            let reason = if target_version.is_some() {
                "unsupported_target_version"
            } else {
                "target_version_unconfirmed"
            };
            log::warn!(
                "[ADVANCED_APPX] phase=online_hook status=skipped reason={}",
                reason
            );
        } else {
            let appx_path =
                lr_core::offline_appx::stage_curated_online_removal_script(target_partition)?;
            if !lr_core::offline_appx::curated_online_script_is_staged(target_partition)? {
                anyhow::bail!("preinstalled application removal script readback mismatch");
            }
            specialize_account_command.push_str(
                &lr_core::offline_appx::render_curated_specialize_command(6)?,
            );
            log::info!(
                "[ADVANCED_APPX] phase=online_hook status=staged path={:?}",
                appx_path
            );
        }
    }

    // 构建 FirstLogonCommands
    // Keep script execution and directory cleanup in one staged launcher. The answer-file field
    // only invokes that fixed launcher, avoiding nested `cmd /s /c` quoting differences while the
    // launcher preserves failures and removes staging only after the PowerShell process exits.
    let first_logon_commands = lr_core::first_logon::render_command(1)?;
    let deploy_specialize_command = String::new();

    // 根据系统版本生成不同的 XML 内容
    let xml_content = if is_win7 {
        // Windows 7 专用无人值守配置
        // Win7 不支持: HideOnlineAccountScreens, HideWirelessSetupInOOBE, SkipMachineOOBE, SkipUserOOBE, HideLocalAccountScreen, HideOEMRegistrationScreen(家庭版)
        generate_win7_unattend_xml(
            &deploy_specialize_command,
            &first_logon_commands,
            arch_str,
            &specialize_account_command,
            &user_accounts,
            &auto_logon,
        )
    } else if is_win8 {
        // Windows 8/8.1 无人值守配置
        // Win8 支持部分 Win10 的选项，但不支持所有
        generate_win8_unattend_xml(
            &deploy_specialize_command,
            &first_logon_commands,
            arch_str,
            &specialize_account_command,
            &user_accounts,
            &auto_logon,
        )
    } else {
        // Windows 10/11 无人值守配置（默认）
        let international = crate::core::dism_exe::DismExe::new()?
            .get_offline_international_settings(target_partition)?;
        log::info!(
            "[UNATTEND] 目标系统国际化设置: UI={}, system={}, user={}, input={}, timezone={}",
            international.ui_language,
            international.system_locale,
            international.user_locale,
            international.input_locale,
            international.time_zone
        );
        generate_win10_unattend_xml(
            &deploy_specialize_command,
            &first_logon_commands,
            arch_str,
            &international,
            &specialize_account_command,
            &user_accounts,
            &auto_logon,
        )
    };

    let panther_dir = format!("{}\\Windows\\Panther", target_partition);
    std::fs::create_dir_all(&panther_dir)?;

    let unattend_path = format!("{}\\unattend.xml", panther_dir);
    std::fs::write(&unattend_path, &xml_content)?;
    log::info!(
        "[UNATTEND] 已写入: {} ({})",
        unattend_path,
        if is_win7 {
            "Win7配置"
        } else if is_win8 {
            "Win8配置"
        } else {
            "Win10/11配置"
        }
    );

    // 同时写入到 Sysprep 目录
    let sysprep_dir = format!("{}\\Windows\\System32\\Sysprep", target_partition);
    if std::path::Path::new(&sysprep_dir).exists() {
        let sysprep_unattend = format!("{}\\unattend.xml", sysprep_dir);
        let _ = std::fs::write(&sysprep_unattend, &xml_content);
        log::info!("[UNATTEND] 已写入: {}", sysprep_unattend);
    }

    Ok(())
}

fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 生成 Windows 7 专用的无人值守配置
///
/// Win7 的 OOBE 配置与 Win10/11 有显著差异：
/// - 不支持 HideOnlineAccountScreens
/// - 不支持 HideWirelessSetupInOOBE
/// - 不支持 SkipMachineOOBE / SkipUserOOBE
/// - 不支持 HideLocalAccountScreen
/// - 不支持 HideOEMRegistrationScreen（家庭版不支持）
/// - 需要设置 NetworkLocation 来跳过网络位置选择
fn generate_win7_unattend_xml(
    deploy_specialize_command: &str,
    first_logon_commands: &str,
    arch: &str,
    specialize_account_command: &str,
    user_accounts: &str,
    auto_logon: &str,
) -> String {
    // Win7 使用最小化的OOBE配置以确保兼容所有版本（包括家庭版）
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <UserData>
                <ProductKey>
                    <WillShowUI>OnError</WillShowUI>
                </ProductKey>
                <AcceptEula>true</AcceptEula>
            </UserData>
        </component>
    </settings>
    <settings pass="specialize">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <ComputerName>*</ComputerName>
        </component>
        <component name="Microsoft-Windows-Deployment" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <RunSynchronous>
                {deploy_specialize_command}
                {specialize_account_command}
            </RunSynchronous>
        </component>
    </settings>
    <settings pass="oobeSystem">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <OOBE>
                <HideEULAPage>true</HideEULAPage>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>
            {user_accounts}
            {auto_logon}
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#,
        arch = arch,
        deploy_specialize_command = deploy_specialize_command,
        first_logon_commands = first_logon_commands,
        specialize_account_command = specialize_account_command,
        user_accounts = user_accounts,
        auto_logon = auto_logon
    )
}

/// 生成 Windows 8/8.1 专用的无人值守配置
///
/// Win8/8.1 支持部分 Win10 的选项：
/// - 支持 HideLocalAccountScreen
/// - 不支持 HideOnlineAccountScreens
/// - 不支持 HideWirelessSetupInOOBE
/// - 不支持 SkipMachineOOBE / SkipUserOOBE
fn generate_win8_unattend_xml(
    deploy_specialize_command: &str,
    first_logon_commands: &str,
    arch: &str,
    specialize_account_command: &str,
    user_accounts: &str,
    auto_logon: &str,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <UserData>
                <ProductKey>
                    <WillShowUI>OnError</WillShowUI>
                </ProductKey>
                <AcceptEula>true</AcceptEula>
            </UserData>
        </component>
    </settings>
    <settings pass="specialize">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <ComputerName>*</ComputerName>
        </component>
        <component name="Microsoft-Windows-Deployment" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <RunSynchronous>
                {deploy_specialize_command}
                {specialize_account_command}
            </RunSynchronous>
        </component>
    </settings>
    <settings pass="oobeSystem">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideLocalAccountScreen>true</HideLocalAccountScreen>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>
            {user_accounts}
            {auto_logon}
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#,
        arch = arch,
        deploy_specialize_command = deploy_specialize_command,
        first_logon_commands = first_logon_commands,
        specialize_account_command = specialize_account_command,
        user_accounts = user_accounts,
        auto_logon = auto_logon
    )
}

/// 生成 Windows 10/11 无人值守配置
///
/// 通过预置 LocalAccount、目标镜像的完整国际化设置和以下 OOBE 选项跳过账户/隐私等屏幕：
/// - HideOnlineAccountScreens
/// - HideWirelessSetupInOOBE
///
/// 注：SkipMachineOOBE / SkipUserOOBE 已被微软弃用且在 Win11 上不可靠，故不再使用。
fn generate_win10_unattend_xml(
    deploy_specialize_command: &str,
    first_logon_commands: &str,
    arch: &str,
    international: &crate::core::dism_exe::OfflineInternationalSettings,
    specialize_account_command: &str,
    user_accounts: &str,
    auto_logon: &str,
) -> String {
    let ui_language = escape_xml_text(&international.ui_language);
    let system_locale = escape_xml_text(&international.system_locale);
    let user_locale = escape_xml_text(&international.user_locale);
    let input_locale = escape_xml_text(&international.input_locale);
    let time_zone = escape_xml_text(&international.time_zone);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <UserData>
                <ProductKey>
                    <WillShowUI>OnError</WillShowUI>
                </ProductKey>
                <AcceptEula>true</AcceptEula>
            </UserData>
        </component>
    </settings>
    <settings pass="specialize">
        <component name="Microsoft-Windows-Deployment" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <RunSynchronous>
                {deploy_specialize_command}
                {specialize_account_command}
            </RunSynchronous>
        </component>
    </settings>
    <settings pass="oobeSystem">
        <component name="Microsoft-Windows-International-Core" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <InputLocale>{input_locale}</InputLocale>
            <SystemLocale>{system_locale}</SystemLocale>
            <UILanguage>{ui_language}</UILanguage>
            <UserLocale>{user_locale}</UserLocale>
        </component>
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <TimeZone>{time_zone}</TimeZone>
            <OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
                <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
                <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
                <ProtectYourPC>3</ProtectYourPC>
            </OOBE>
            {user_accounts}
            {auto_logon}
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#,
        arch = arch,
        deploy_specialize_command = deploy_specialize_command,
        first_logon_commands = first_logon_commands,
        specialize_account_command = specialize_account_command,
        user_accounts = user_accounts,
        auto_logon = auto_logon,
        input_locale = input_locale,
        system_locale = system_locale,
        ui_language = ui_language,
        user_locale = user_locale,
        time_zone = time_zone
    )
}

#[cfg(test)]
mod workflow_session_tests {
    use super::*;

    #[test]
    fn existing_account_mode_removes_only_new_install_account_and_first_logon_options() {
        let mut config = crate::core::config::InstallConfig {
            unattended: true,
            custom_unattend_file: "custom.xml".to_string(),
            custom_username: "NewUser".to_string(),
            migrate_wifi: true,
            remove_uwp_apps: true,
            disable_windows_defender: true,
            disable_reserved_storage: true,
            preinstalled_software_config: "authenticated-selection".to_string(),
            disable_uac: true,
            ..crate::core::config::InstallConfig::default()
        };
        config.builtin_administrator.enabled = true;
        config.builtin_administrator.password = "secret".into();

        assert!(suppress_new_install_only_options(&mut config));
        assert!(!config.unattended);
        assert!(config.custom_unattend_file.is_empty());
        assert!(config.custom_username.is_empty());
        assert!(!config.builtin_administrator.enabled);
        assert!(config.builtin_administrator.password.is_empty());
        assert!(!config.migrate_wifi);
        assert!(!config.remove_uwp_apps);
        assert!(!config.disable_windows_defender);
        assert!(!config.disable_reserved_storage);
        assert!(config.preinstalled_software_config.is_empty());
        assert!(
            config.disable_uac,
            "independent offline tweaks stay requested"
        );
    }

    #[test]
    fn stale_private_pe_cleanup_is_bounded_and_preserves_foreign_content() {
        let workspace = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "pe-install-stale-private-residue",
        )
        .unwrap();
        let root = workspace.path().join("LetRecovery_PE");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("boot.wim"), b"stale-wim").unwrap();
        std::fs::write(root.join("pe_guid.txt"), b"stale-journal").unwrap();
        std::fs::write(root.join("handoff-config-123-4.ini"), b"stale-config").unwrap();
        std::fs::write(root.join("keep.txt"), b"user-content").unwrap();

        assert_eq!(cleanup_private_pe_residue_root(&root).unwrap(), 3);
        assert!(!root.join("boot.wim").exists());
        assert!(!root.join("pe_guid.txt").exists());
        assert!(!root.join("handoff-config-123-4.ini").exists());
        assert_eq!(
            std::fs::read(root.join("keep.txt")).unwrap(),
            b"user-content"
        );
        assert!(root.exists());
    }

    #[test]
    fn stale_private_pe_cleanup_removes_empty_product_directory() {
        let workspace = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "pe-install-empty-private-residue",
        )
        .unwrap();
        let root = workspace.path().join("LetRecovery_PE");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("boot-01234567-abcd.wim"), b"stale-wim").unwrap();
        std::fs::write(root.join("pe_pending.txt"), b"stale-journal").unwrap();

        assert_eq!(cleanup_private_pe_residue_root(&root).unwrap(), 2);
        assert!(!root.exists());
    }

    #[test]
    fn interactive_completion_warning_waits_for_the_user_instead_of_rebooting() {
        assert!(!should_reboot_after_completion_warning(true, false));
        assert!(!should_reboot_after_completion_warning(false, false));
        assert!(should_reboot_after_completion_warning(true, true));
        assert!(should_reboot_after_completion_warning(false, true));
    }

    #[test]
    fn windows_11_unattend_fully_specifies_international_oobe() {
        let international = crate::core::dism_exe::OfflineInternationalSettings {
            ui_language: "zh-CN".to_string(),
            system_locale: "zh-CN".to_string(),
            user_locale: "zh-CN".to_string(),
            input_locale: "0804:00000804".to_string(),
            time_zone: "China Standard Time".to_string(),
        };
        let mut security_ui_command = lr_core::sec_health_ui::render_specialize_command(3).unwrap();
        security_ui_command
            .push_str(&lr_core::reserved_storage::render_specialize_command(4).unwrap());
        let first_logon_commands = lr_core::first_logon::render_command(1).unwrap();
        let deploy_specialize_command = String::new();
        let xml = generate_win10_unattend_xml(
            &deploy_specialize_command,
            &first_logon_commands,
            "amd64",
            &international,
            &security_ui_command,
            "<UserAccounts><AdministratorPassword><Value>test</Value><PlainText>true</PlainText></AdministratorPassword></UserAccounts>",
            "<AutoLogon><Enabled>true</Enabled><Username>Administrator</Username></AutoLogon>",
        );
        assert!(xml.contains("<UILanguage>zh-CN</UILanguage>"));
        assert!(xml.contains("<InputLocale>0804:00000804</InputLocale>"));
        assert!(xml.contains("<TimeZone>China Standard Time</TimeZone>"));
        assert!(xml.contains("<HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>"));
        assert!(!xml.contains("<HideLocalAccountScreen>"));
        assert!(!xml.contains("<ComputerName>*</ComputerName>"));
        assert!(!xml.contains("<SkipMachineOOBE>"));
        assert!(!xml.contains("<SkipUserOOBE>"));
        assert!(xml.contains("remove-sec-health-ui.ps1"));
        assert!(xml.contains("<Order>3</Order>"));
        assert!(xml.contains("disable-reserved-storage.ps1"));
        assert!(xml.contains("<Order>4</Order>"));
        let order3 = xml.find("<Order>3</Order>").unwrap();
        let order4 = xml.find("<Order>4</Order>").unwrap();
        assert!(order3 < order4);
        assert!(!xml.contains("remove-onedrive-win32.ps1"));
        let decode_xml_text = |value: &str| {
            value
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
        };
        let paths = xml
            .split("<Path>")
            .skip(1)
            .map(|tail| tail.split_once("</Path>").unwrap().0)
            .map(decode_xml_text)
            .collect::<Vec<_>>();
        assert!(paths.iter().all(|path| {
            path.encode_utf16().count() <= lr_core::unattend_command::RUN_SYNCHRONOUS_PATH_MAX_UTF16
        }));
        let command_lines = xml
            .split("<CommandLine>")
            .skip(1)
            .map(|tail| tail.split_once("</CommandLine>").unwrap().0)
            .map(decode_xml_text)
            .collect::<Vec<_>>();
        assert_eq!(command_lines.len(), 1);
        assert!(command_lines[0].contains(lr_core::first_logon::LAUNCHER_FILE_NAME));
        assert!(command_lines[0].contains("cmd.exe /d /c"));
        assert!(!command_lines[0].contains("powershell.exe"));
        assert!(
            command_lines[0].encode_utf16().count()
                <= lr_core::unattend_command::FIRST_LOGON_COMMAND_LINE_MAX_UTF16
        );

        let without_optional_hooks = generate_win10_unattend_xml(
            &deploy_specialize_command,
            "",
            "amd64",
            &international,
            "",
            "<UserAccounts><AdministratorPassword><Value>test</Value><PlainText>true</PlainText></AdministratorPassword></UserAccounts>",
            "<AutoLogon><Enabled>true</Enabled><Username>Administrator</Username></AutoLogon>",
        );
        assert!(!without_optional_hooks.contains("remove-onedrive-win32.ps1"));
        assert!(!without_optional_hooks.contains("<Order>5</Order>"));
    }

    fn disconnected_session() -> (WorkflowSession, Sender<WorkerMessage>) {
        let (tx, rx) = channel();
        (
            WorkflowSession {
                progress_state: Arc::new(Mutex::new(ProgressState::new_install())),
                message_rx: Some(rx),
                started: true,
                worker_handle: None,
                worker_finished: false,
                terminal_message_seen: false,
                channel_failure_reported: false,
                operation_type: Some(OperationType::Install),
                authenticated_handoff: None,
                workflow_journal: None,
            },
            tx,
        )
    }

    #[test]
    fn worker_poll_yields_before_a_progress_flood_can_starve_ui_timers() {
        let (mut session, tx) = WorkflowSession::new_message_preview(OperationType::Install);
        for index in 0..(MAX_WORKER_MESSAGES_PER_POLL * 2) {
            tx.send(WorkerMessage::SetStatus(format!("flood-{index}")))
                .unwrap();
        }
        tx.send(WorkerMessage::SetInstallStep(InstallStep::ApplyImage))
            .unwrap();

        session.process_messages();
        assert!(session
            .message_rx
            .as_ref()
            .is_some_and(|receiver| receiver.try_recv().is_ok()));

        for _ in 0..(MAX_WORKER_MESSAGES_PER_POLL * 2 + 1) {
            session.process_messages();
            if session.snapshot().has_current_step {
                break;
            }
        }
        let state = session.snapshot();
        assert!(state.has_current_step);
        assert_eq!(state.current_install_step, InstallStep::ApplyImage);
    }

    #[test]
    fn worker_poll_collapses_tool_samples_to_the_latest_atomic_update() {
        let (mut session, tx) = WorkflowSession::new_message_preview(OperationType::Install);
        tx.send(WorkerMessage::SetInstallStep(InstallStep::ImportDrivers))
            .unwrap();
        for percentage in 1..=80 {
            tx.send(WorkerMessage::SetProgressStatus {
                progress: percentage,
                status: format!("driver-sample-{percentage}"),
            })
            .unwrap();
        }

        session.process_messages();

        let state = session.snapshot();
        assert_eq!(state.current_install_step, InstallStep::ImportDrivers);
        assert_eq!(state.step_progress, 80);
        assert_eq!(state.status_message, "driver-sample-80");
    }

    #[test]
    fn progress_coalescing_preserves_step_boundary_order() {
        let (mut session, tx) = WorkflowSession::new_message_preview(OperationType::Install);
        tx.send(WorkerMessage::SetInstallStep(InstallStep::VerifyImage))
            .unwrap();
        tx.send(WorkerMessage::SetProgress(100)).unwrap();
        tx.send(WorkerMessage::SetInstallStep(InstallStep::FormatPartition))
            .unwrap();
        tx.send(WorkerMessage::SetProgress(25)).unwrap();

        session.process_messages();

        let state = session.snapshot();
        assert_eq!(state.current_install_step, InstallStep::FormatPartition);
        assert_eq!(state.step_progress, 25);
    }

    #[test]
    fn unexpected_worker_disconnect_becomes_a_terminal_failure() {
        let (mut session, tx) = disconnected_session();
        drop(tx);

        session.process_messages();

        let state = session.snapshot();
        assert!(state.is_failed);
        assert_eq!(state.error_message, Some(tr!("工作线程异常终止")));
        assert!(session.terminal_message_seen);
        assert!(session.channel_failure_reported);
    }

    #[test]
    fn disconnect_after_completed_does_not_replace_the_terminal_result() {
        let (mut session, tx) = disconnected_session();
        tx.send(WorkerMessage::Completed).unwrap();
        drop(tx);

        session.process_messages();

        let state = session.snapshot();
        assert!(state.is_completed);
        assert!(!state.is_failed);
        assert!(!session.channel_failure_reported);
    }
}
