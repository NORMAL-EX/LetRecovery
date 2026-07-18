use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::core::config::{ConfigFileManager, OperationType};
use crate::core::dism::DismProgress;
use crate::tr;
use crate::ui::progress::{BackupStep, InstallStep, ProgressState};
use crate::utils::reboot_pe;
use crate::workflow_journal::PeWorkflowJournal;
use crate::workflow_journal::RecoveryCheckpointSnapshot;

/// 递归查找目录中的所有 CAB 文件
fn find_cab_files_in_directory(dir: &str) -> Vec<PathBuf> {
    let mut cab_files = Vec::new();
    find_cab_files_recursive(Path::new(dir), &mut cab_files);
    cab_files
}

/// 递归搜索 CAB 文件的辅助函数
fn find_cab_files_recursive(dir: &Path, cab_files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().to_lowercase() == "cab" {
                        cab_files.push(path);
                    }
                }
            } else if path.is_dir() {
                find_cab_files_recursive(&path, cab_files);
            }
        }
    }
}

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
    /// 标记完成
    Completed,
    /// 标记失败
    Failed(String),
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
    pub(crate) fn new_for_operation(operation_type: Option<OperationType>) -> Self {
        let workflow_journal = operation_type.and_then(|operation_type| {
            match PeWorkflowJournal::create(operation_type) {
                Ok(journal) => journal,
                Err(error) => {
                    log::warn!("[CHECKPOINT] 无法创建工作流检查点，将继续原流程: {}", error);
                    None
                }
            }
        });

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
            workflow_journal,
        }
    }

    /// 启动工作线程
    pub(crate) fn start_worker(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        let (tx, rx) = channel::<WorkerMessage>();
        self.message_rx = Some(rx);

        let operation_type = self.operation_type;

        self.worker_handle = Some(thread::spawn(move || match operation_type {
            Some(OperationType::Install) => {
                execute_install_workflow(tx);
            }
            Some(OperationType::Backup) => {
                crate::workflows::execute_backup_workflow(tx);
            }
            Some(OperationType::Expand) => {
                crate::workflows::execute_expand_workflow(tx);
            }
            None => {
                let _ = tx.send(WorkerMessage::Failed(tr!("未检测到安装或备份配置")));
            }
        }));
    }

    /// 处理工作线程消息
    pub(crate) fn process_messages(&mut self) {
        let mut disconnected = false;
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
                if matches!(msg, WorkerMessage::Completed | WorkerMessage::Failed(_)) {
                    self.terminal_message_seen = true;
                }
                if let Some(journal) = self.workflow_journal.as_mut() {
                    let result = match &msg {
                        WorkerMessage::SetInstallStep(step) => journal.observe_install_step(*step),
                        WorkerMessage::SetBackupStep(step) => journal.observe_backup_step(*step),
                        WorkerMessage::Completed => journal.complete(),
                        WorkerMessage::Failed(error) => journal.fail(error),
                        WorkerMessage::SetProgress(_) | WorkerMessage::SetStatus(_) => Ok(()),
                    };
                    if let Err(error) = result {
                        log::warn!("[CHECKPOINT] 记录工作流状态失败，将继续原流程: {}", error);
                    }
                }
                if let Ok(mut state) = self.progress_state.lock() {
                    match msg {
                        WorkerMessage::SetInstallStep(step) => {
                            state.set_install_step(step);
                        }
                        WorkerMessage::SetBackupStep(step) => {
                            state.set_backup_step(step);
                        }
                        WorkerMessage::SetProgress(p) => {
                            state.set_step_progress(p);
                        }
                        WorkerMessage::SetStatus(s) => {
                            state.status_message = s;
                        }
                        WorkerMessage::Completed => {
                            state.mark_completed();
                        }
                        WorkerMessage::Failed(e) => {
                            state.mark_failed(&e);
                        }
                    }
                }
            }
        }
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

/// 执行安装工作流
fn execute_install_workflow(tx: Sender<WorkerMessage>) {
    use crate::core::bcdedit::BootManager;
    use crate::core::disk::DiskManager;
    use crate::core::dism::Dism;
    use crate::core::ghost::Ghost;
    use crate::ui::advanced_options::apply_advanced_options;

    log::info!("========== 开始PE安装流程 ==========");
    // 注：BitLocker 透传解锁已在 main() 最前面统一执行（早于操作类型检测），这里不再重复。

    // 查找并校验本次安装任务（marker 与配置需匹配）。
    let (data_partition, target_partition, config) = match ConfigFileManager::find_install_task() {
        Ok(task) => task,
        Err(e) => {
            let _ = tx.send(WorkerMessage::Failed(tr!("读取安装任务失败: {}", e)));
            return;
        }
    };

    log::info!("数据分区: {}", data_partition);
    let _ = tx.send(WorkerMessage::SetStatus(tr!(
        "数据分区: {}",
        data_partition
    )));

    // 切换到正常系统端选定的镜像引擎（随重启传入），使 PE 端使用相同引擎
    lr_core::set_active_engine(lr_core::WimEngine::from_u8(config.wim_engine));

    log::info!("目标分区: {}", config.target_partition);
    log::info!("镜像文件: {}", config.image_path);

    let data_dir = ConfigFileManager::get_data_dir(&data_partition);
    let resolved_source = if config.is_xp_i386 {
        ConfigFileManager::resolve_staged_xp_source(
            &data_dir,
            &config.image_path,
            &config.xp_source_arch,
        )
    } else {
        ConfigFileManager::resolve_staged_file(&data_dir, &config.image_path)
    };
    let image_path = match resolved_source {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(error) => {
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "安装配置中的镜像文件名无效: {}",
                error
            )));
            return;
        }
    };

    if !std::path::Path::new(&image_path).exists() {
        let _ = tx.send(WorkerMessage::Failed(tr!("镜像文件不存在: {}", image_path)));
        return;
    }

    log::info!("完整镜像路径: {}", image_path);

    // Step 0: 校验镜像完整性（WIM/ESD）。放在格式化之前——镜像损坏就提前失败，
    // 不会白白格式化目标盘，也能给出明确“镜像损坏”而不是释放到一半才崩。
    // GHO 不是 WIM，跳过 wimlib 校验。
    let xp_custom_sif = if config.is_xp_i386 && !config.custom_unattend_file.is_empty() {
        match ConfigFileManager::resolve_staged_file(&data_dir, &config.custom_unattend_file) {
            Ok(path) => Some(path),
            Err(error) => {
                let _ = tx.send(WorkerMessage::Failed(tr!(
                    "自定义 XP 应答文件名无效: {}",
                    error
                )));
                return;
            }
        }
    } else {
        None
    };
    if config.is_xp_i386 {
        if let Err(error) =
            lr_core::xp_i386::validate_i386_source(std::path::Path::new(&image_path))
        {
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "XP/2003 安装源校验失败: {}",
                error
            )));
            return;
        }
    }

    if !config.is_gho && !config.is_xp_i386 {
        let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::VerifyImage));
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "正在校验系统镜像完整性（可能需要几分钟）..."
        )));
        log::info!("[PE安装] 开始校验镜像: {}", image_path);

        let (verify_tx, verify_rx) = channel::<DismProgress>();
        let tx_v = tx.clone();
        let verify_handle = thread::spawn(move || {
            while let Ok(progress) = verify_rx.recv() {
                let _ = tx_v.send(WorkerMessage::SetProgress(progress.percentage));
                let _ = tx_v.send(WorkerMessage::SetStatus(progress.status));
            }
        });

        let verify_result = Dism::new().verify_image(&image_path, Some(verify_tx));
        let _ = verify_handle.join();

        if let Err(e) = verify_result {
            log::error!("[PE安装] 镜像校验失败: {}", e);
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "镜像校验失败：镜像可能已损坏或不完整（{}）。请重新获取镜像后重试。",
                e
            )));
            return;
        }
        log::info!("[PE安装] 镜像校验通过");
        let _ = tx.send(WorkerMessage::SetProgress(100));
    } else {
        log::info!("[PE安装] GHO 镜像，跳过 wimlib 校验");
    }

    // Auto mode can become UEFI after a partitioning script, so preflight all
    // modes except explicit Legacy before the first target-disk mutation.
    let staged_pca_compat =
        match crate::core::pca_preflight::staged_config(&config, std::path::Path::new(&data_dir)) {
            Ok(staged) => staged,
            Err(error) => {
                let _ = tx.send(WorkerMessage::Failed(error));
                return;
            }
        };
    let pca_compat_package = if config.is_xp_i386 {
        None
    } else {
        match crate::core::pca_preflight::verify_before_disk_write(
            &image_path,
            config.volume_index,
            config.is_gho,
            config.is_xp,
            config.boot_mode != 2,
            config.boot_pca_mode,
            staged_pca_compat.as_ref(),
        ) {
            Ok(package) => package,
            Err(error) => {
                let _ = tx.send(WorkerMessage::Failed(error));
                return;
            }
        }
    };

    // 装机前运行 diskpart 脚本（分区准备）——来自数据目录暂存的 diskpart\
    if config.run_diskpart_scripts {
        let _ = tx.send(WorkerMessage::SetStatus(tr!("正在运行 Diskpart 脚本...")));
        let scripts_dir = std::path::Path::new(&data_dir).join("diskpart");
        log::info!("[PE安装] 运行 Diskpart 脚本: {}", scripts_dir.display());
        match lr_core::diskpart::run_scripts_in_dir(&scripts_dir) {
            Ok(out) => log::info!("[PE安装] Diskpart 脚本执行完成:\n{}", out),
            Err(e) => {
                log::error!("[PE安装] Diskpart 脚本执行失败: {}", e);
                let _ = tx.send(WorkerMessage::Failed(tr!("Diskpart 脚本执行失败: {}", e)));
                return;
            }
        }
    }

    // Step 1: 格式化分区
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::FormatPartition));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在格式化目标分区...")));

    // 使用卷标参数（如果有配置的话）
    let volume_label = if config.volume_label.is_empty() {
        None
    } else {
        Some(config.volume_label.as_str())
    };

    match DiskManager::format_partition_with_label(&target_partition, volume_label) {
        Ok(_) => {
            log::info!("分区格式化成功");
            let _ = tx.send(WorkerMessage::SetProgress(100));
        }
        Err(e) => {
            log::error!("[PE安装] 格式化分区失败: {}", e);
            let _ = tx.send(WorkerMessage::Failed(tr!("格式化分区失败: {}", e)));
            return;
        }
    }

    // Step 2: 释放镜像
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::ApplyImage));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在释放系统镜像...")));

    if config.is_xp_i386 {
        let _ = tx.send(WorkerMessage::SetStatus(tr!(
            "正在准备 XP/2003 文本模式安装..."
        )));
        match lr_core::xp_i386::install_from_i386(
            std::path::Path::new(&image_path),
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
        let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Cleanup));
        if let Err(error) =
            DiskManager::cleanup_auto_created_partition_and_extend(&target_partition)
        {
            log::error!("[PE安装/XP文本模式] 清理自动数据分区失败: {error}");
            let _ = tx.send(WorkerMessage::Failed(tr!(
                "清理安装临时分区并合并空间失败: {}",
                error
            )));
            return;
        }
        ConfigFileManager::cleanup_all(&data_partition, &target_partition);
        let _ = tx.send(WorkerMessage::SetProgress(100));
        let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Complete));
        let _ = tx.send(WorkerMessage::Completed);
        std::thread::sleep(std::time::Duration::from_secs(3));
        reboot_pe();
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

    let apply_result = if config.is_gho {
        // GHO镜像使用Ghost
        let ghost = Ghost::new();
        if !ghost.is_available() {
            let _ = tx.send(WorkerMessage::Failed(tr!("Ghost工具不可用")));
            return;
        }

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
        dism.apply_image(
            &image_path,
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
    let _ = tx.send(WorkerMessage::SetProgress(100));

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
                let _ = tx_driver.send(WorkerMessage::SetProgress(progress.percentage));
                let _ = tx_driver.send(WorkerMessage::SetStatus(tr!(
                    "导入驱动: {}",
                    progress.status
                )));
            }
        });

        let dism = Dism::new();
        match dism.add_drivers_offline_with_progress(
            &apply_dir,
            &driver_path,
            Some(driver_progress_tx),
        ) {
            Ok(_) => {
                log::info!("驱动导入成功");
            }
            Err(e) => {
                log::warn!("导入驱动失败: {}", e);
                // 不中断安装流程，继续执行
            }
        }

        // 等待进度监控线程结束
        let _ = driver_progress_handle.join();

        // 同时检查驱动目录中是否有 CAB 文件并安装
        let cab_files_in_driver_dir = find_cab_files_in_directory(&driver_path);
        if !cab_files_in_driver_dir.is_empty() {
            log::info!(
                "在驱动目录中发现 {} 个 CAB 文件，将一并安装",
                cab_files_in_driver_dir.len()
            );
            let _ = tx.send(WorkerMessage::SetStatus(tr!(
                "正在安装驱动目录中的 {} 个 CAB 更新包...",
                cab_files_in_driver_dir.len()
            )));

            // 创建进度通道
            let (cab_progress_tx, cab_progress_rx) = channel::<DismProgress>();
            let tx_cab = tx.clone();

            // 启动进度监控线程
            let cab_progress_handle = thread::spawn(move || {
                while let Ok(progress) = cab_progress_rx.recv() {
                    let _ = tx_cab.send(WorkerMessage::SetProgress(progress.percentage));
                    let _ = tx_cab.send(WorkerMessage::SetStatus(tr!(
                        "安装CAB: {}",
                        progress.status
                    )));
                }
            });

            let dism = Dism::new();
            match dism.add_packages_offline_from_dir(
                &apply_dir,
                &driver_path,
                Some(cab_progress_tx),
            ) {
                Ok((success, fail)) => {
                    log::info!("驱动目录中的CAB安装完成: {} 成功, {} 失败", success, fail);
                }
                Err(e) => {
                    log::warn!("驱动目录中的CAB安装失败: {}", e);
                }
            }

            let _ = cab_progress_handle.join();
        }
    } else if config.should_import_drivers() && !driver_path_exists {
        log::info!("驱动目录不存在，跳过驱动导入: {}", driver_path);
        let _ = tx.send(WorkerMessage::SetStatus(tr!("跳过驱动导入（目录不存在）")));
    } else if config.has_driver_data() {
        // SaveOnly 模式：驱动已保存但不导入
        let _ = tx.send(WorkerMessage::SetStatus(tr!("跳过驱动导入（仅保存模式）")));
        log::info!("驱动操作模式为仅保存，跳过驱动导入");
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
        let cab_path = format!("{}\\updates", data_dir);
        if std::path::Path::new(&cab_path).exists() {
            let _ = tx.send(WorkerMessage::SetStatus(tr!("正在安装更新包...")));

            // 创建进度通道
            let (cab_progress_tx, cab_progress_rx) = channel::<DismProgress>();
            let tx_cab = tx.clone();

            // 启动进度监控线程
            let cab_progress_handle = thread::spawn(move || {
                while let Ok(progress) = cab_progress_rx.recv() {
                    let _ = tx_cab.send(WorkerMessage::SetProgress(progress.percentage));
                    let _ = tx_cab.send(WorkerMessage::SetStatus(tr!(
                        "安装更新: {}",
                        progress.status
                    )));
                }
            });

            let dism = Dism::new();
            match dism.add_packages_offline_from_dir(&apply_dir, &cab_path, Some(cab_progress_tx)) {
                Ok((success, fail)) => {
                    log::info!("CAB更新包安装完成: {} 成功, {} 失败", success, fail);
                    let _ = tx.send(WorkerMessage::SetStatus(tr!(
                        "更新包安装完成: {} 成功, {} 失败",
                        success,
                        fail
                    )));
                }
                Err(e) => {
                    log::warn!("CAB更新包安装失败: {}", e);
                    // 不中断安装流程，继续执行
                }
            }

            // 等待进度监控线程结束
            let _ = cab_progress_handle.join();
        } else {
            log::info!("更新包目录不存在，跳过CAB安装: {}", cab_path);
            let _ = tx.send(WorkerMessage::SetStatus(tr!(
                "跳过更新包安装（目录不存在）"
            )));
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
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在修复引导...")));

    let boot_manager = BootManager::new();
    let use_uefi = DiskManager::resolve_install_uefi_mode(config.boot_mode, &target_partition);

    // XP/2003 写 ntldr 引导；其余走 bcdboot。
    // XP 判定：配置已标记 或 释放后的系统缺少 \Windows\Boot（该目录仅 Vista+ 才有）。
    let win_boot_dir = format!("{}\\Windows\\Boot", target_partition);
    let is_xp = config.is_xp || !std::path::Path::new(&win_boot_dir).exists();
    let boot_result = if is_xp {
        if use_uefi {
            log::info!("[PE安装] 识别为 XP/2003 + UEFI，写入 XP UEFI/GPT 引导");
            // UEFI 化映像：用映像自带 bootxp64.efi/BCC 写 UEFI 引导；
            // 失败（如映像非 UEFI 化、缺引导文件）则回退 Legacy(ntldr)。
            match boot_manager.write_xp_uefi_gpt_boot(&target_partition) {
                Ok(()) => Ok(()),
                Err(e) => {
                    log::warn!("[PE安装] XP UEFI 引导失败({})，回退 Legacy(ntldr)", e);
                    let _ = tx.send(WorkerMessage::SetStatus(tr!(
                        "XP UEFI 引导不可用，回退 Legacy 引导..."
                    )));
                    boot_manager.write_xp_boot(&target_partition)
                }
            }
        } else {
            log::info!("[PE安装] 识别为 XP/2003(Legacy)，写入 XP 引导(ntldr/boot.ini)");
            boot_manager.write_xp_boot(&target_partition)
        }
    } else {
        boot_manager.repair_boot_advanced(&target_partition, use_uefi, config.boot_pca_mode)
    };
    if let Err(e) = boot_result {
        let _ = tx.send(WorkerMessage::Failed(tr!("修复引导失败: {}", e)));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 6: 应用高级选项
    let _ = tx.send(WorkerMessage::SetInstallStep(
        InstallStep::ApplyAdvancedOptions,
    ));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在应用高级选项...")));

    if let Err(e) = apply_advanced_options(&target_partition, &config) {
        log::warn!("应用高级选项失败: {}", e);
    }
    // 注入数据分区上的用户驱动（bin/drivers/<版本> 由正常端复制而来）
    crate::ui::advanced_options::inject_user_drivers_from_data(&target_partition, &data_dir);
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 7: 生成无人值守配置
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::GenerateUnattend));

    if config.unattended {
        if !config.custom_unattend_file.is_empty() {
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

    // 离线登录兜底：放开空密码登录策略 +（已知用户名时）配置空密码自动登录。
    // 解决整盘备份/未 sysprep 镜像下 unattend 不生效、登录界面退化为"其他用户"的问题。
    if let Err(e) = crate::core::account_fix::ensure_offline_login(
        &target_partition,
        &config.custom_username,
        config.is_gho || config.is_xp,
    ) {
        log::warn!("离线登录兜底设置失败（不影响安装）: {}", e);
    } else {
        log::info!("[LOGIN] 已应用离线登录兜底设置");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 8: 清理临时文件
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Cleanup));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理临时文件...")));

    // 清理自动创建的数据分区并扩展目标分区
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理自动创建的分区...")));
    if let Err(e) = DiskManager::cleanup_auto_created_partition_and_extend(&target_partition) {
        log::error!("清理自动创建分区失败: {}", e);
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "清理安装临时分区并合并空间失败: {}",
            e
        )));
        return;
    }
    log::info!("自动创建分区清理完成");
    let _ = tx.send(WorkerMessage::SetProgress(50));

    // 只有分区清理成功后才删除数据目录和诊断材料，避免先删 marker 后无法安全重试。
    ConfigFileManager::cleanup_all(&data_partition, &target_partition);
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // 完成
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Complete));
    let _ = tx.send(WorkerMessage::Completed);

    log::info!("========== PE安装流程完成 ==========");

    // PE环境下安装完成后强制重启
    log::info!("即将重启...");
    std::thread::sleep(std::time::Duration::from_secs(3));
    reboot_pe();
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
    use crate::ui::advanced_options::get_scripts_dir_name;
    use std::path::Path;

    let username = if config.custom_username.is_empty() {
        escape_xml_text("User")
    } else {
        escape_xml_text(&config.custom_username)
    };

    let scripts_dir = get_scripts_dir_name();

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
    let (is_win7, is_win8) = match get_file_version(&ntdll_path) {
        Some((major, minor, build, _)) => {
            log::info!(
                "[UNATTEND] 检测到目标系统版本 (ntdll.dll): {}.{}.{}",
                major,
                minor,
                build
            );

            let is_win7 = major == 6 && minor == 1;
            let is_win8 = major == 6 && (minor == 2 || minor == 3);
            (is_win7, is_win8)
        }
        None => {
            log::warn!(
                "[UNATTEND] 无法读取 ntdll.dll 版本: {:?}, 默认使用 Win10/11 配置",
                ntdll_path
            );
            (false, false)
        }
    };

    // 构建 FirstLogonCommands
    let mut first_logon_commands = String::new();
    let mut order = 1;

    // 首次登录脚本（如果存在）
    first_logon_commands.push_str(&format!(r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>cmd /c if exist %SystemDrive%\{}\firstlogon.bat call %SystemDrive%\{}\firstlogon.bat</CommandLine>
                    <Description>Run first login script</Description>
                </SynchronousCommand>"#, order, scripts_dir, scripts_dir));
    order += 1;

    // 如果需要删除UWP应用（仅 Win10/11 支持）
    if config.remove_uwp_apps && !is_win7 && !is_win8 {
        first_logon_commands.push_str(&format!(r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>powershell -ExecutionPolicy Bypass -File %SystemDrive%\{}\remove_uwp.ps1</CommandLine>
                    <Description>Remove preinstalled UWP apps</Description>
                </SynchronousCommand>"#, order, scripts_dir));
        order += 1;
    }

    // 清理脚本目录（最后执行）
    first_logon_commands.push_str(&format!(
        r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>cmd /c rd /s /q %SystemDrive%\{}</CommandLine>
                    <Description>Cleanup scripts directory</Description>
                </SynchronousCommand>"#,
        order, scripts_dir
    ));

    // 根据系统版本生成不同的 XML 内容
    let xml_content = if is_win7 {
        // Windows 7 专用无人值守配置
        // Win7 不支持: HideOnlineAccountScreens, HideWirelessSetupInOOBE, SkipMachineOOBE, SkipUserOOBE, HideLocalAccountScreen, HideOEMRegistrationScreen(家庭版)
        generate_win7_unattend_xml(&username, scripts_dir, &first_logon_commands, arch_str)
    } else if is_win8 {
        // Windows 8/8.1 无人值守配置
        // Win8 支持部分 Win10 的选项，但不支持所有
        generate_win8_unattend_xml(&username, scripts_dir, &first_logon_commands, arch_str)
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
            &username,
            scripts_dir,
            &first_logon_commands,
            arch_str,
            &international,
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
    username: &str,
    scripts_dir: &str,
    first_logon_commands: &str,
    arch: &str,
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
                <RunSynchronousCommand wcm:action="add">
                    <Order>1</Order>
                    <Path>cmd /c if exist %SystemDrive%\{scripts_dir}\deploy.bat call %SystemDrive%\{scripts_dir}\deploy.bat</Path>
                    <Description>Run custom deploy script</Description>
                </RunSynchronousCommand>
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
            <UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password>
                            <Value></Value>
                            <PlainText>true</PlainText>
                        </Password>
                        <Description>Local User</Description>
                        <DisplayName>{username}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{username}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>
            <AutoLogon>
                <Password>
                    <Value></Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{username}</Username>
            </AutoLogon>
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#,
        arch = arch,
        scripts_dir = scripts_dir,
        username = username,
        first_logon_commands = first_logon_commands
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
    username: &str,
    scripts_dir: &str,
    first_logon_commands: &str,
    arch: &str,
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
                <RunSynchronousCommand wcm:action="add">
                    <Order>1</Order>
                    <Path>cmd /c if exist %SystemDrive%\{scripts_dir}\deploy.bat call %SystemDrive%\{scripts_dir}\deploy.bat</Path>
                    <Description>Run custom deploy script</Description>
                </RunSynchronousCommand>
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
            <UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password>
                            <Value></Value>
                            <PlainText>true</PlainText>
                        </Password>
                        <Description>Local User</Description>
                        <DisplayName>{username}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{username}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>
            <AutoLogon>
                <Password>
                    <Value></Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{username}</Username>
            </AutoLogon>
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#,
        arch = arch,
        scripts_dir = scripts_dir,
        username = username,
        first_logon_commands = first_logon_commands
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
    username: &str,
    scripts_dir: &str,
    first_logon_commands: &str,
    arch: &str,
    international: &crate::core::dism_exe::OfflineInternationalSettings,
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
                <RunSynchronousCommand wcm:action="add">
                    <Order>1</Order>
                    <Path>cmd /c if exist %SystemDrive%\{scripts_dir}\deploy.bat call %SystemDrive%\{scripts_dir}\deploy.bat</Path>
                    <Description>Run custom deploy script</Description>
                </RunSynchronousCommand>
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
            <UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password>
                            <Value></Value>
                            <PlainText>true</PlainText>
                        </Password>
                        <Description>Local User</Description>
                        <DisplayName>{username}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{username}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>
            <AutoLogon>
                <Password>
                    <Value></Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{username}</Username>
            </AutoLogon>
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#,
        arch = arch,
        scripts_dir = scripts_dir,
        username = username,
        first_logon_commands = first_logon_commands,
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
    fn windows_11_unattend_fully_specifies_international_oobe() {
        let international = crate::core::dism_exe::OfflineInternationalSettings {
            ui_language: "zh-CN".to_string(),
            system_locale: "zh-CN".to_string(),
            user_locale: "zh-CN".to_string(),
            input_locale: "0804:00000804".to_string(),
            time_zone: "China Standard Time".to_string(),
        };
        let xml = generate_win10_unattend_xml(
            "测试用户",
            "LetRecoveryScripts",
            "",
            "amd64",
            &international,
        );
        assert!(xml.contains("<UILanguage>zh-CN</UILanguage>"));
        assert!(xml.contains("<InputLocale>0804:00000804</InputLocale>"));
        assert!(xml.contains("<TimeZone>China Standard Time</TimeZone>"));
        assert!(xml.contains("<HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>"));
        assert!(!xml.contains("<HideLocalAccountScreen>"));
        assert!(!xml.contains("<ComputerName>*</ComputerName>"));
        assert!(!xml.contains("<SkipMachineOOBE>"));
        assert!(!xml.contains("<SkipUserOOBE>"));
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
                workflow_journal: None,
            },
            tx,
        )
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
