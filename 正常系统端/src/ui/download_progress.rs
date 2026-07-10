use egui;
use std::sync::mpsc;

use crate::app::App;
use crate::download::aria2::{Aria2Manager, DownloadProgress, DownloadStatus};
use crate::tr;
use lr_core::download_integrity::{
    select_expected_hash, validate_download_filename, validate_download_url, DownloadTransport,
    ExpectedHash, HashAlgorithm, HashVerification, IntegrityRequirement,
};

/// 下载控制命令
#[derive(Debug, Clone)]
pub enum DownloadCommand {
    Pause,
    Resume,
    Cancel,
}

/// Download integrity verification state.
#[derive(Debug, Clone, PartialEq)]
pub enum IntegrityVerifyState {
    /// 未开始
    NotStarted,
    /// 正在校验
    Verifying { algorithm: HashAlgorithm },
    /// Metadata did not provide any checksum.
    NotProvided,
    /// 校验通过
    Passed { algorithm: HashAlgorithm },
    /// 校验失败
    Failed {
        algorithm: HashAlgorithm,
        expected: String,
        actual: String,
    },
    /// 校验出错
    Error {
        algorithm: HashAlgorithm,
        message: String,
    },
}

/// 静态命令发送器（用于跨线程通信）
static DOWNLOAD_CMD_SENDER: std::sync::Mutex<Option<mpsc::Sender<DownloadCommand>>> =
    std::sync::Mutex::new(None);

/// Download integrity result receiver.
static INTEGRITY_VERIFY_RX: std::sync::Mutex<Option<mpsc::Receiver<IntegrityVerifyState>>> =
    std::sync::Mutex::new(None);

impl App {
    pub fn show_download_progress(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr!("下载进度"));
        ui.separator();

        // 从channel接收进度更新
        self.update_download_progress();

        // Poll the checksum worker without blocking the UI thread.
        self.check_integrity_verify_result();

        // 如果有待下载的任务，开始下载
        if let Some(url) = self.pending_download_url.take() {
            let filename = self.pending_download_filename.take();
            let save_path = if self.download_save_path.is_empty() {
                crate::utils::path::get_exe_dir()
                    .join("downloads")
                    .to_string_lossy()
                    .to_string()
            } else {
                self.download_save_path.clone()
            };

            // 创建下载目录
            let _ = std::fs::create_dir_all(&save_path);

            // 检查是否为PE下载（pe_download_then_action不为None时为PE下载）
            let is_pe_download = self.pe_download_then_action.is_some();

            // 记录MD5设置情况
            if is_pe_download {
                if let Some(ref sha256) = self.pending_pe_sha256 {
                    log::info!("[DOWNLOAD] PE download has a SHA-256 checksum: {}", sha256);
                } else if let Some(ref md5) = self.pending_pe_md5 {
                    log::info!("[下载] PE下载已设置MD5校验值: {}", md5);
                } else {
                    log::warn!(
                        "[DOWNLOAD] PE metadata has no checksum; verification will be skipped"
                    );
                }
            }

            // 初始化 aria2 并开始下载
            self.start_download_task_with_pe_check(
                &url,
                &save_path,
                filename.as_deref(),
                is_pe_download,
            );
        }

        // 显示初始化错误
        if let Some(ref error) = self.download_init_error {
            ui.add_space(15.0);
            ui.colored_label(egui::Color32::RED, tr!("错误: {}", error));
            ui.add_space(10.0);
            if ui.button(tr!("返回")).clicked() {
                self.download_init_error = None;
                // 先获取待执行操作
                let action = self.pe_download_then_action.take();
                // 根据操作类型返回对应页面
                match action {
                    Some(crate::app::PeDownloadThenAction::Install) => {
                        self.current_panel = crate::app::Panel::SystemInstall;
                    }
                    Some(crate::app::PeDownloadThenAction::Backup) => {
                        self.current_panel = crate::app::Panel::SystemBackup;
                    }
                    Some(crate::app::PeDownloadThenAction::Expand) => {
                        self.current_panel = crate::app::Panel::Tools;
                    }
                    None => {
                        self.current_panel = crate::app::Panel::OnlineDownload;
                    }
                }
            }
            return;
        }

        // 克隆需要的数据以避免借用冲突
        let progress_clone = self.download_progress.clone();
        let filename_clone = self.current_download_filename.clone();
        let integrity_verify_state = self.integrity_verify_state.clone();

        // 显示当前下载状态
        if let Some(progress) = progress_clone {
            ui.add_space(15.0);

            // 文件名
            if let Some(filename) = &filename_clone {
                ui.label(tr!("文件: {}", filename));
            }

            // 进度条
            ui.add(
                egui::ProgressBar::new(progress.percentage as f32 / 100.0)
                    .show_percentage()
                    .animate(progress.status == DownloadStatus::Active),
            );

            // 详细信息
            ui.horizontal(|ui| {
                ui.label(tr!(
                    "已下载: {} / {}",
                    Self::format_bytes(progress.completed_length),
                    Self::format_bytes(progress.total_length)
                ));
                ui.separator();
                ui.label(tr!(
                    "速度: {}/s",
                    Self::format_bytes(progress.download_speed)
                ));
            });

            // 状态
            let status_text = match &progress.status {
                DownloadStatus::Waiting => "等待中...",
                DownloadStatus::Active => "下载中...",
                DownloadStatus::Paused => "已暂停",
                DownloadStatus::Complete => "下载完成",
                DownloadStatus::Error(msg) => msg.as_str(),
            };
            ui.label(tr!("状态: {}", status_text));

            ui.add_space(15.0);

            // 控制按钮 - 使用克隆的状态来判断
            let status = progress.status.clone();
            let is_complete = status == DownloadStatus::Complete;
            let is_error = matches!(status, DownloadStatus::Error(_));

            ui.horizontal(|ui| {
                match status {
                    DownloadStatus::Active => {
                        if ui.button(tr!("暂停")).clicked() {
                            self.pause_current_download();
                        }
                    }
                    DownloadStatus::Paused => {
                        if ui.button(tr!("继续")).clicked() {
                            self.resume_current_download();
                        }
                    }
                    DownloadStatus::Complete => {
                        // A declared checksum is mandatory; missing metadata remains a
                        // separate compatibility state and is never reported as passed.
                        match &integrity_verify_state {
                            IntegrityVerifyState::NotStarted => {
                                if self.pe_download_then_action.is_some() {
                                    match select_expected_hash(
                                        self.pending_pe_sha256.as_deref(),
                                        self.pending_pe_md5.as_deref(),
                                    ) {
                                        Ok(IntegrityRequirement::Required(expected)) => {
                                            let algorithm = expected.algorithm();
                                            let filename = self
                                                .current_download_filename
                                                .clone()
                                                .unwrap_or_default();
                                            let file_path =
                                                std::path::Path::new(&self.download_save_path)
                                                    .join(filename);
                                            log::info!(
                                                "[INTEGRITY] Verifying {} with {}",
                                                file_path.display(),
                                                algorithm.name()
                                            );
                                            self.start_integrity_verify(file_path, expected);
                                            self.integrity_verify_state =
                                                IntegrityVerifyState::Verifying { algorithm };
                                        }
                                        Ok(IntegrityRequirement::NotProvided) => {
                                            log::warn!(
                                                "[INTEGRITY] PE metadata has no checksum; continuing for legacy compatibility"
                                            );
                                            self.integrity_verify_state =
                                                IntegrityVerifyState::NotProvided;
                                        }
                                        Err(error) => {
                                            let algorithm = error.algorithm;
                                            log::error!(
                                                "[INTEGRITY] Invalid declared {} checksum: {}",
                                                algorithm.name(),
                                                error
                                            );
                                            self.remove_rejected_download();
                                            self.integrity_verify_state =
                                                IntegrityVerifyState::Error {
                                                    algorithm,
                                                    message: error.to_string(),
                                                };
                                        }
                                    }
                                } else {
                                    self.integrity_verify_state =
                                        IntegrityVerifyState::NotProvided;
                                }
                            }
                            IntegrityVerifyState::Verifying { algorithm } => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(tr!(
                                        "正在使用 {} 校验文件完整性，请稍候...",
                                        algorithm.name()
                                    ));
                                });
                            }
                            IntegrityVerifyState::Passed { .. }
                            | IntegrityVerifyState::NotProvided => {
                                match &integrity_verify_state {
                                    IntegrityVerifyState::Passed { algorithm } => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(102, 187, 106),
                                            tr!("{} 校验通过，下载完成！", algorithm.name()),
                                        );
                                    }
                                    IntegrityVerifyState::NotProvided
                                        if self.pe_download_then_action.is_some() =>
                                    {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 165, 0),
                                            tr!("未提供文件校验值，已跳过完整性校验"),
                                        );
                                    }
                                    _ => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(102, 187, 106),
                                            tr!("下载完成！"),
                                        );
                                    }
                                }

                                self.pending_pe_md5 = None;
                                self.pending_pe_sha256 = None;

                                // 检查是否需要下载后跳转到安装页面（系统镜像）
                                if self.download_then_install {
                                    // 获取下载的文件路径
                                    if let Some(ref downloaded_path) = self.download_then_install_path {
                                        let path = downloaded_path.clone();
                                        self.local_image_path = path.clone();

                                        // 检查是否是小白模式自动安装
                                        let is_easy_mode_auto = self.easy_mode_auto_install;

                                        // 清理下载状态
                                        self.download_then_install = false;
                                        self.download_then_install_path = None;
                                        self.cleanup_download();

                                        if is_easy_mode_auto {
                                            // 小白模式：直接开始安装
                                            ui.label(tr!("正在准备自动安装..."));
                                            log::info!("[EASY MODE] 下载完成，自动开始安装流程");

                                            // 重置自动安装标志
                                            self.easy_mode_auto_install = false;

                                            // 加载镜像信息
                                            self.load_image_volumes();

                                            // 需要等待镜像信息加载完成后再开始安装
                                            // 设置一个标志表示需要在镜像加载完成后自动开始安装
                                            self.easy_mode_pending_auto_start = true;

                                            // 跳转到安装页面（安装页面会检测pending标志并自动开始）
                                            self.current_panel = crate::app::Panel::SystemInstall;
                                        } else {
                                            // 普通模式：跳转到安装页面
                                            ui.label(tr!("正在跳转到安装页面..."));
                                            self.current_panel = crate::app::Panel::SystemInstall;
                                            // 加载镜像信息
                                            self.load_image_volumes();
                                        }
                                    } else {
                                        self.download_then_install = false;
                                        self.easy_mode_auto_install = false;
                                        self.cleanup_download();
                                        self.current_panel = crate::app::Panel::SystemInstall;
                                    }
                                }
                                // 检查是否需要下载后运行软件
                                else if self.soft_download_then_run {
                                    ui.label(tr!("正在启动软件..."));

                                    if let Some(ref run_path) = self.soft_download_then_run_path {
                                        let path = run_path.clone();
                                        // 清理下载状态
                                        self.soft_download_then_run = false;
                                        self.soft_download_then_run_path = None;
                                        self.cleanup_download();

                                        // 运行软件
                                        if let Err(e) = std::process::Command::new(&path).spawn() {
                                            log::warn!("启动软件失败: {}", e);
                                        }

                                        // 返回在线下载页面
                                        self.current_panel = crate::app::Panel::OnlineDownload;
                                    } else {
                                        self.soft_download_then_run = false;
                                        self.cleanup_download();
                                        self.current_panel = crate::app::Panel::OnlineDownload;
                                    }
                                }
                                // 检查是否有待继续的PE操作
                                else if self.pe_download_then_action.is_some() {
                                    ui.label(tr!("正在准备继续操作..."));
                                    // 延迟一帧后继续操作，避免状态冲突
                                    let action = self.pe_download_then_action.take();
                                    self.cleanup_download();

                                    match action {
                                        Some(crate::app::PeDownloadThenAction::Install) => {
                                            // 继续安装
                                            self.start_installation();
                                        }
                                        Some(crate::app::PeDownloadThenAction::Backup) => {
                                            // 继续备份，并切换到备份进度页面
                                            self.start_backup_internal();
                                            self.current_panel = crate::app::Panel::BackupProgress;
                                        }
                                        Some(crate::app::PeDownloadThenAction::Expand) => {
                                            // 继续无损扩大C盘交接，并返回工具箱页面
                                            self.current_panel = crate::app::Panel::Tools;
                                            self.start_expand_pe_handoff();
                                        }
                                        None => {
                                            self.current_panel = crate::app::Panel::OnlineDownload;
                                        }
                                    }
                                } else if ui.button(tr!("返回")).clicked() {
                                    self.cleanup_download();
                                    self.current_panel = crate::app::Panel::OnlineDownload;
                                }
                            }
                            IntegrityVerifyState::Failed {
                                algorithm,
                                expected,
                                actual,
                            } => {
                                ui.colored_label(
                                    egui::Color32::RED,
                                    tr!("文件校验失败！文件可能已损坏。"),
                                );
                                ui.add_space(5.0);
                                ui.label(tr!("预期{}: {}", algorithm.name(), expected));
                                ui.label(tr!("实际{}: {}", algorithm.name(), actual));
                                ui.add_space(10.0);

                                if ui.button(tr!("返回重新下载")).clicked() {
                                    self.return_after_rejected_pe_download();
                                }
                            }
                            IntegrityVerifyState::Error { algorithm, message } => {
                                ui.colored_label(
                                    egui::Color32::RED,
                                    tr!(
                                        "{} 校验出错，已停止使用该文件: {}",
                                        algorithm.name(),
                                        message
                                    ),
                                );
                                ui.add_space(10.0);

                                if ui.button(tr!("返回重新下载")).clicked() {
                                    self.return_after_rejected_pe_download();
                                }
                            }
                        }
                    }
                    DownloadStatus::Error(_) => {
                        if ui.button(tr!("返回")).clicked() {
                            // 先获取待执行操作
                            let action = self.pe_download_then_action.take();
                            self.cleanup_download();
                            // 根据操作类型返回对应页面
                            match action {
                                Some(crate::app::PeDownloadThenAction::Install) => {
                                    self.current_panel = crate::app::Panel::SystemInstall;
                                }
                                Some(crate::app::PeDownloadThenAction::Backup) => {
                                    self.current_panel = crate::app::Panel::SystemBackup;
                                }
                                Some(crate::app::PeDownloadThenAction::Expand) => {
                                    self.current_panel = crate::app::Panel::Tools;
                                }
                                None => {
                                    self.current_panel = crate::app::Panel::OnlineDownload;
                                }
                            }
                        }
                    }
                    _ => {}
                }

                if !is_complete && !is_error
                    && ui.button(tr!("取消")).clicked() {
                        self.cancel_current_download();
                    }
            });
        } else {
            // 显示等待状态或无任务
            if self.current_download.is_some() {
                ui.add_space(15.0);
                ui.label(tr!("正在初始化下载..."));
                ui.spinner();
            } else {
                ui.label(tr!("没有正在进行的下载任务"));
                if ui.button(tr!("返回")).clicked() {
                    self.current_panel = crate::app::Panel::OnlineDownload;
                }
            }
        }
    }

    /// Run checksum verification outside the UI thread.
    fn start_integrity_verify(&self, file_path: std::path::PathBuf, expected: ExpectedHash) {
        let (tx, rx) = mpsc::channel::<IntegrityVerifyState>();

        *INTEGRITY_VERIFY_RX.lock().unwrap() = Some(rx);

        std::thread::spawn(move || {
            let algorithm = expected.algorithm();
            let start_time = std::time::Instant::now();

            match lr_core::download_integrity::verify_file(&file_path, &expected) {
                Ok(HashVerification::Passed { .. }) => {
                    let elapsed = start_time.elapsed();
                    log::info!(
                        "[INTEGRITY] {} verification passed in {:?}",
                        algorithm.name(),
                        elapsed
                    );
                    let _ = tx.send(IntegrityVerifyState::Passed { algorithm });
                }
                Ok(HashVerification::Mismatch {
                    expected, actual, ..
                }) => {
                    log::error!(
                        "[INTEGRITY] {} mismatch; expected {}, actual {}",
                        algorithm.name(),
                        expected,
                        actual
                    );
                    let _ = tx.send(IntegrityVerifyState::Failed {
                        algorithm,
                        expected,
                        actual,
                    });
                }
                Err(error) => {
                    log::error!(
                        "[INTEGRITY] Failed to calculate {}: {}",
                        algorithm.name(),
                        error
                    );
                    let _ = tx.send(IntegrityVerifyState::Error {
                        algorithm,
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    /// Poll the checksum worker and fail closed when a declared hash cannot be
    /// verified. The rejected file is removed exactly once here.
    fn check_integrity_verify_result(&mut self) {
        let mut guard = INTEGRITY_VERIFY_RX.lock().unwrap();
        if let Some(ref rx) = *guard {
            if let Ok(state) = rx.try_recv() {
                if matches!(
                    &state,
                    IntegrityVerifyState::Failed { .. } | IntegrityVerifyState::Error { .. }
                ) {
                    self.remove_rejected_download();
                }
                self.integrity_verify_state = state;
                *guard = None;
            }
        }
    }

    fn remove_rejected_download(&self) {
        let Some(filename) = self.current_download_filename.as_deref() else {
            return;
        };
        let file_path = std::path::Path::new(&self.download_save_path).join(filename);
        match std::fs::remove_file(&file_path) {
            Ok(()) => log::info!(
                "[INTEGRITY] Removed rejected download: {}",
                file_path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "[INTEGRITY] Could not remove rejected download {}: {}",
                file_path.display(),
                error
            ),
        }
    }

    fn return_after_rejected_pe_download(&mut self) {
        let action = self.pe_download_then_action.take();
        self.cleanup_download();
        self.current_panel = match action {
            Some(crate::app::PeDownloadThenAction::Install) => crate::app::Panel::SystemInstall,
            Some(crate::app::PeDownloadThenAction::Backup) => crate::app::Panel::SystemBackup,
            Some(crate::app::PeDownloadThenAction::Expand) => crate::app::Panel::Tools,
            None => crate::app::Panel::OnlineDownload,
        };
    }

    /// 从channel更新下载进度
    fn update_download_progress(&mut self) {
        if let Some(ref rx) = self.download_progress_rx {
            // 非阻塞接收所有可用的进度更新
            while let Ok(progress) = rx.try_recv() {
                // 保存gid
                if self.download_gid.is_none() && !progress.gid.is_empty() {
                    self.download_gid = Some(progress.gid.clone());
                }
                self.download_progress = Some(progress);
            }
        }
    }

    /// 启动下载任务（带PE检查）
    ///
    /// 优化：URL解析和aria2启动并行执行，大幅减少初始化时间
    fn start_download_task_with_pe_check(
        &mut self,
        url: &str,
        save_path: &str,
        filename: Option<&str>,
        is_pe_download: bool,
    ) {
        if let Some(filename) = filename {
            if let Err(error) = validate_download_filename(filename) {
                self.download_init_error = Some(tr!("下载文件名不安全或无效: {}", error));
                return;
            }
        }
        let allow_insecure_http = self.app_config.allow_insecure_http_downloads;
        let validated_url = match validate_download_url(url, allow_insecure_http) {
            Ok(url) => url,
            Err(error) => {
                self.download_init_error = Some(tr!("下载地址不安全或无效: {}", error));
                return;
            }
        };
        if validated_url.transport() == DownloadTransport::InsecureHttp {
            log::warn!(
                "[SECURITY] Insecure HTTP download explicitly enabled for {}",
                validated_url.as_str()
            );
        }
        let url = validated_url.into_string();

        self.current_download_filename = filename.map(|s| s.to_string());
        self.current_download = Some(url.clone());
        self.download_init_error = None;
        self.download_gid = None;
        self.integrity_verify_state = IntegrityVerifyState::NotStarted;

        // 创建进度通道
        let (progress_tx, progress_rx) = mpsc::channel::<DownloadProgress>();
        self.download_progress_rx = Some(progress_rx);

        // 创建控制通道
        let (cmd_tx, cmd_rx) = mpsc::channel::<DownloadCommand>();

        // 清空旧的下载管理器状态
        {
            let mut guard = self.download_manager.lock().unwrap();
            *guard = None;
        }

        // 克隆需要的数据
        let save_path = save_path.to_string();
        let filename = filename.map(|s| s.to_string());

        // 存储命令发送器
        self.store_download_command_sender(cmd_tx);

        // 在后台线程中执行下载
        std::thread::spawn(move || {
            // 创建新的tokio运行时
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = progress_tx.send(DownloadProgress {
                        gid: String::new(),
                        completed_length: 0,
                        total_length: 0,
                        download_speed: 0,
                        percentage: 0.0,
                        status: DownloadStatus::Error(tr!("创建运行时失败: {}", e)),
                    });
                    return;
                }
            };

            rt.block_on(async move {
                let init_start = std::time::Instant::now();
                log::info!("[下载] 开始并行初始化...");

                // ===== 核心优化：并行执行URL解析和aria2启动 =====
                let url_for_resolve = url.clone();

                // 任务1：解析PE下载URL（如果需要）
                let url_resolve_task = async {
                    if is_pe_download {
                        log::info!("[下载] 检测到PE下载，开始解析下载链接");
                        match crate::download::pe_url_resolver::resolve_pe_download_url(
                            &url_for_resolve,
                        )
                        .await
                        {
                            Ok(result) => {
                                log::info!("[下载] PE下载链接解析成功: {}", result.download_url);
                                log::info!("[下载] 解析到的headers数量: {}", result.headers.len());
                                for (i, h) in result.headers.iter().enumerate() {
                                    let header_name = h.split(':').next().unwrap_or("Unknown");
                                    log::info!("[下载] 接收到Header[{}]: {}", i, header_name);
                                }
                                (result.download_url, Some(result.headers))
                            }
                            Err(e) => {
                                log::warn!("[下载] PE下载链接解析失败: {}，使用原始链接", e);
                                (url_for_resolve.clone(), None)
                            }
                        }
                    } else {
                        (url_for_resolve.clone(), None)
                    }
                };

                // 任务2：启动aria2（与URL解析同时进行）
                let aria2_start_task = async {
                    log::info!("[下载] 启动aria2...");
                    Aria2Manager::start().await
                };

                // 并行执行两个任务
                let ((final_url, headers), aria2_result) =
                    tokio::join!(url_resolve_task, aria2_start_task);

                let init_elapsed = init_start.elapsed();
                log::info!("[下载] 并行初始化完成，总耗时: {:?}", init_elapsed);

                let final_url = match validate_download_url(&final_url, allow_insecure_http) {
                    Ok(url) => {
                        if url.transport() == DownloadTransport::InsecureHttp {
                            log::warn!(
                                "[SECURITY] Resolver returned an explicitly allowed HTTP URL: {}",
                                url.as_str()
                            );
                        }
                        url.into_string()
                    }
                    Err(error) => {
                        let _ = progress_tx.send(DownloadProgress {
                            gid: String::new(),
                            completed_length: 0,
                            total_length: 0,
                            download_speed: 0,
                            percentage: 0.0,
                            status: DownloadStatus::Error(tr!("下载地址不安全或无效: {}", error)),
                        });
                        return;
                    }
                };

                // 检查aria2启动结果
                let aria2 = match aria2_result {
                    Ok(manager) => manager,
                    Err(e) => {
                        let _ = progress_tx.send(DownloadProgress {
                            gid: String::new(),
                            completed_length: 0,
                            total_length: 0,
                            download_speed: 0,
                            percentage: 0.0,
                            status: DownloadStatus::Error(tr!("初始化aria2失败: {}", e)),
                        });
                        return;
                    }
                };

                // 添加下载任务（根据是否有headers选择方法）
                log::info!("[下载] 准备添加下载任务，检查headers状态...");
                let gid = match headers {
                    Some(hdrs) if !hdrs.is_empty() => {
                        log::info!(
                            "[下载] 使用带headers的下载方法，headers数量: {}",
                            hdrs.len()
                        );
                        for (i, h) in hdrs.iter().enumerate() {
                            let header_name = h.split(':').next().unwrap_or("Unknown");
                            log::info!("[下载] 传递Header[{}]: {}", i, header_name);
                        }
                        aria2
                            .add_download_with_headers(
                                &final_url,
                                &save_path,
                                filename.as_deref(),
                                Some(hdrs),
                            )
                            .await
                    }
                    Some(_hdrs) => {
                        log::warn!("[下载] headers为空列表，使用普通下载方法");
                        aria2
                            .add_download(&final_url, &save_path, filename.as_deref())
                            .await
                    }
                    _ => {
                        log::info!("[下载] 无headers，使用普通下载方法");
                        aria2
                            .add_download(&final_url, &save_path, filename.as_deref())
                            .await
                    }
                };

                let gid = match gid {
                    Ok(gid) => gid,
                    Err(e) => {
                        let _ = progress_tx.send(DownloadProgress {
                            gid: String::new(),
                            completed_length: 0,
                            total_length: 0,
                            download_speed: 0,
                            percentage: 0.0,
                            status: DownloadStatus::Error(tr!("添加任务失败: {}", e)),
                        });
                        return;
                    }
                };

                // 定期获取进度并发送，同时监听控制命令
                // 连续查询失败计数：单次超时/网络抖动不再立即中断（aria2c 是独立进程仍在
                // 后台下载，--continue 保证不丢进度），重试多次仍失败才报错退出，
                // 避免“进度卡死且无法暂停”。
                let mut consecutive_errors: u32 = 0;
                const MAX_CONSECUTIVE_ERRORS: u32 = 8;

                loop {
                    // 处理控制命令（非阻塞）。即使上一轮 get_status 超时，循环也会回到这里，
                    // 从而保证暂停/恢复/取消命令能被及时消费（修复“暂停也暂停不了”）。
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        match cmd {
                            DownloadCommand::Pause => {
                                let _ = aria2.pause(&gid).await;
                            }
                            DownloadCommand::Resume => {
                                let _ = aria2.resume(&gid).await;
                            }
                            DownloadCommand::Cancel => {
                                let _ = aria2.cancel(&gid).await;
                                return;
                            }
                        }
                    }

                    // 获取进度
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

                    match aria2.get_status(&gid).await {
                        Ok(progress) => {
                            consecutive_errors = 0;
                            let is_complete = progress.status == DownloadStatus::Complete;
                            let is_error = matches!(progress.status, DownloadStatus::Error(_));

                            if progress_tx.send(progress).is_err() {
                                break; // 接收端已关闭
                            }

                            if is_complete || is_error {
                                break;
                            }
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            log::warn!(
                                "[DOWNLOAD] 获取下载状态失败({}/{})，将重试: {}",
                                consecutive_errors,
                                MAX_CONSECUTIVE_ERRORS,
                                e
                            );
                            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                let _ = progress_tx.send(DownloadProgress {
                                    gid: gid.clone(),
                                    completed_length: 0,
                                    total_length: 0,
                                    download_speed: 0,
                                    percentage: 0.0,
                                    status: DownloadStatus::Error(tr!("获取状态失败: {}", e)),
                                });
                                break;
                            }
                            // 未达上限：继续轮询重试，不中断循环
                            continue;
                        }
                    }
                }
            });
        });
    }

    /// 启动下载任务（不带PE检查，用于非PE下载）
    fn start_download_task(&mut self, url: &str, save_path: &str, filename: Option<&str>) {
        self.start_download_task_with_pe_check(url, save_path, filename, false);
    }

    /// 存储下载命令发送器
    fn store_download_command_sender(&mut self, _sender: mpsc::Sender<DownloadCommand>) {
        *DOWNLOAD_CMD_SENDER.lock().unwrap() = Some(_sender);
    }

    fn pause_current_download(&mut self) {
        if let Some(ref sender) = *DOWNLOAD_CMD_SENDER.lock().unwrap() {
            let _ = sender.send(DownloadCommand::Pause);
        }
    }

    fn resume_current_download(&mut self) {
        if let Some(ref sender) = *DOWNLOAD_CMD_SENDER.lock().unwrap() {
            let _ = sender.send(DownloadCommand::Resume);
        }
    }

    fn cancel_current_download(&mut self) {
        // 分别短暂加锁，避免同时持有两个 static 锁导致嵌套/重入死锁
        {
            let mut sender_guard = DOWNLOAD_CMD_SENDER.lock().unwrap();
            if let Some(ref sender) = *sender_guard {
                let _ = sender.send(DownloadCommand::Cancel);
            }
            *sender_guard = None;
        }
        *INTEGRITY_VERIFY_RX.lock().unwrap() = None;

        // 先获取待执行操作
        let action = self.pe_download_then_action.take();
        let was_download_then_install = self.download_then_install;
        self.cleanup_download();

        // 根据操作类型返回对应页面
        if was_download_then_install {
            self.current_panel = crate::app::Panel::OnlineDownload;
        } else {
            match action {
                Some(crate::app::PeDownloadThenAction::Install) => {
                    self.current_panel = crate::app::Panel::SystemInstall;
                }
                Some(crate::app::PeDownloadThenAction::Backup) => {
                    self.current_panel = crate::app::Panel::SystemBackup;
                }
                Some(crate::app::PeDownloadThenAction::Expand) => {
                    self.current_panel = crate::app::Panel::Tools;
                }
                None => {
                    self.current_panel = crate::app::Panel::OnlineDownload;
                }
            }
        }
    }

    /// 清理下载状态
    fn cleanup_download(&mut self) {
        self.download_progress = None;
        self.current_download = None;
        self.download_gid = None;
        self.download_progress_rx = None;
        self.current_download_filename = None;
        self.pe_download_then_action = None;
        self.download_then_install = false;
        self.download_then_install_path = None;
        self.soft_download_then_run = false;
        self.soft_download_then_run_path = None;
        self.pending_pe_md5 = None;
        self.pending_pe_sha256 = None;
        self.integrity_verify_state = IntegrityVerifyState::NotStarted;

        // 分别短暂加锁，避免同时持有两个 static 锁导致嵌套/重入死锁
        *DOWNLOAD_CMD_SENDER.lock().unwrap() = None;
        *INTEGRITY_VERIFY_RX.lock().unwrap() = None;
    }

    /// 格式化字节数
    fn format_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }
}
