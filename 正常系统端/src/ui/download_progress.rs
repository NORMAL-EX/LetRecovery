use egui;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;

use crate::app::App;
use crate::download::aria2::{Aria2Manager, DownloadProgress, DownloadStatus};

/// 下载控制命令
#[derive(Debug, Clone)]
pub enum DownloadCommand {
    Pause,
    Resume,
    Cancel,
}

/// MD5校验状态
#[derive(Debug, Clone, PartialEq)]
pub enum Md5VerifyState {
    /// 未开始
    NotStarted,
    /// 正在校验
    Verifying,
    /// 校验通过
    Passed,
    /// 校验失败
    Failed { expected: String, actual: String },
    /// 校验出错
    Error(String),
}

/// 静态命令发送器（用于跨线程通信）
static mut DOWNLOAD_CMD_SENDER: Option<mpsc::Sender<DownloadCommand>> = None;

/// MD5校验结果接收器
static mut MD5_VERIFY_RX: Option<mpsc::Receiver<Md5VerifyState>> = None;

impl App {
    pub fn show_download_progress(&mut self, ui: &mut egui::Ui) {
        ui.heading("下载进度");
        ui.separator();

        // 从channel接收进度更新
        self.update_download_progress();
        
        // 检查MD5校验结果
        self.check_md5_verify_result();

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
                if let Some(ref md5) = self.pending_pe_md5 {
                    log::info!("[下载] PE下载已设置MD5校验值: {}", md5);
                } else {
                    log::warn!("[下载] PE下载未设置MD5校验值，将跳过校验");
                }
            }
            
            // 初始化 aria2 并开始下载
            self.start_download_task_with_pe_check(&url, &save_path, filename.as_deref(), is_pe_download);
        }

        // 显示初始化错误
        if let Some(ref error) = self.download_init_error {
            ui.add_space(15.0);
            ui.colored_label(egui::Color32::RED, format!("错误: {}", error));
            ui.add_space(10.0);
            if ui.button("返回").clicked() {
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
        let md5_verify_state = self.md5_verify_state.clone();

        // 显示当前下载状态
        if let Some(progress) = progress_clone {
            ui.add_space(15.0);

            // 文件名
            if let Some(filename) = &filename_clone {
                ui.label(format!("文件: {}", filename));
            }

            // 进度条
            ui.add(
                egui::ProgressBar::new(progress.percentage as f32 / 100.0)
                    .show_percentage()
                    .animate(progress.status == DownloadStatus::Active),
            );

            // 详细信息
            ui.horizontal(|ui| {
                ui.label(format!(
                    "已下载: {} / {}",
                    Self::format_bytes(progress.completed_length),
                    Self::format_bytes(progress.total_length)
                ));
                ui.separator();
                ui.label(format!(
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
            ui.label(format!("状态: {}", status_text));

            ui.add_space(15.0);

            // 控制按钮 - 使用克隆的状态来判断
            let status = progress.status.clone();
            let is_complete = status == DownloadStatus::Complete;
            let is_error = matches!(status, DownloadStatus::Error(_));

            ui.horizontal(|ui| {
                match status {
                    DownloadStatus::Active => {
                        if ui.button("暂停").clicked() {
                            self.pause_current_download();
                        }
                    }
                    DownloadStatus::Paused => {
                        if ui.button("继续").clicked() {
                            self.resume_current_download();
                        }
                    }
                    DownloadStatus::Complete => {
                        // 检查MD5校验状态
                        match &md5_verify_state {
                            Md5VerifyState::NotStarted => {
                                // 检查是否需要进行MD5校验（仅PE下载）
                                if self.pending_pe_md5.is_some() && self.pe_download_then_action.is_some() {
                                    ui.label("准备校验文件完整性...");
                                    
                                    // 启动异步MD5校验
                                    let expected_md5 = self.pending_pe_md5.clone().unwrap();
                                    let filename = self.current_download_filename.clone().unwrap_or_default();
                                    let file_path = format!("{}\\{}", self.download_save_path, filename);
                                    
                                    log::info!("[MD5] 开始校验文件: {}", file_path);
                                    log::info!("[MD5] 预期MD5: {}", expected_md5);
                                    
                                    self.start_md5_verify(&file_path, &expected_md5);
                                    self.md5_verify_state = Md5VerifyState::Verifying;
                                } else {
                                    // 没有MD5校验，直接显示完成
                                    if self.pending_pe_md5.is_none() && self.pe_download_then_action.is_some() {
                                        log::warn!("[MD5] PE下载完成但未设置MD5，跳过校验");
                                    }
                                    self.md5_verify_state = Md5VerifyState::Passed;
                                }
                            }
                            Md5VerifyState::Verifying => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("正在校验文件完整性，请稍候...");
                                });
                            }
                            Md5VerifyState::Passed => {
                                ui.colored_label(egui::Color32::GREEN, "✓ 下载完成！");
                                
                                // 清除MD5校验值
                                self.pending_pe_md5 = None;
                                
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
                                            ui.label("正在准备自动安装...");
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
                                            ui.label("正在跳转到安装页面...");
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
                                    ui.label("正在启动软件...");
                                    
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
                                    ui.label("正在准备继续操作...");
                                    // 延迟一帧后继续操作，避免状态冲突
                                    let action = self.pe_download_then_action.take();
                                    self.cleanup_download();
                                    
                                    match action {
                                        Some(crate::app::PeDownloadThenAction::Install) => {
                                            // 继续安装
                                            self.start_installation();
                                        }
                                        Some(crate::app::PeDownloadThenAction::Backup) => {
                                            // 继续备份
                                            self.start_backup_internal();
                                        }
                                        None => {
                                            self.current_panel = crate::app::Panel::OnlineDownload;
                                        }
                                    }
                                } else {
                                    if ui.button("返回").clicked() {
                                        self.cleanup_download();
                                        self.current_panel = crate::app::Panel::OnlineDownload;
                                    }
                                }
                            }
                            Md5VerifyState::Failed { expected, actual } => {
                                // MD5校验失败
                                ui.colored_label(
                                    egui::Color32::RED, 
                                    "✗ 文件校验失败！文件可能已损坏。"
                                );
                                ui.add_space(5.0);
                                ui.label(format!("预期MD5: {}", expected));
                                ui.label(format!("实际MD5: {}", actual));
                                ui.add_space(10.0);
                                
                                // 注意：删除文件的操作已移到 check_md5_verify_result() 中
                                // 避免在 UI 渲染循环中重复执行
                                
                                if ui.button("返回重新下载").clicked() {
                                    // 清理状态
                                    let action = self.pe_download_then_action.take();
                                    self.pending_pe_md5 = None;
                                    self.cleanup_download();
                                    
                                    // 返回对应页面
                                    match action {
                                        Some(crate::app::PeDownloadThenAction::Install) => {
                                            self.current_panel = crate::app::Panel::SystemInstall;
                                        }
                                        Some(crate::app::PeDownloadThenAction::Backup) => {
                                            self.current_panel = crate::app::Panel::SystemBackup;
                                        }
                                        None => {
                                            self.current_panel = crate::app::Panel::OnlineDownload;
                                        }
                                    }
                                }
                            }
                            Md5VerifyState::Error(err) => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(255, 165, 0),
                                    format!("⚠ 校验出错: {}", err)
                                );
                                ui.add_space(5.0);
                                ui.label("文件可能正常，但无法验证完整性。");
                                ui.add_space(10.0);
                                
                                if ui.button("继续使用").clicked() {
                                    self.pending_pe_md5 = None;
                                    self.md5_verify_state = Md5VerifyState::Passed;
                                }
                                
                                if ui.button("返回").clicked() {
                                    let action = self.pe_download_then_action.take();
                                    self.cleanup_download();
                                    match action {
                                        Some(crate::app::PeDownloadThenAction::Install) => {
                                            self.current_panel = crate::app::Panel::SystemInstall;
                                        }
                                        Some(crate::app::PeDownloadThenAction::Backup) => {
                                            self.current_panel = crate::app::Panel::SystemBackup;
                                        }
                                        None => {
                                            self.current_panel = crate::app::Panel::OnlineDownload;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    DownloadStatus::Error(_) => {
                        if ui.button("返回").clicked() {
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
                                None => {
                                    self.current_panel = crate::app::Panel::OnlineDownload;
                                }
                            }
                        }
                    }
                    _ => {}
                }

                if !is_complete && !is_error {
                    if ui.button("取消").clicked() {
                        self.cancel_current_download();
                    }
                }
            });
        } else {
            // 显示等待状态或无任务
            if self.current_download.is_some() {
                ui.add_space(15.0);
                ui.label("正在初始化下载...");
                ui.spinner();
            } else {
                ui.label("没有正在进行的下载任务");
                if ui.button("返回").clicked() {
                    self.current_panel = crate::app::Panel::OnlineDownload;
                }
            }
        }
    }

    /// 启动异步MD5校验
    fn start_md5_verify(&self, file_path: &str, expected_md5: &str) {
        let file_path = file_path.to_string();
        let expected_md5 = expected_md5.to_string();
        
        let (tx, rx) = mpsc::channel::<Md5VerifyState>();
        
        unsafe {
            MD5_VERIFY_RX = Some(rx);
        }
        
        std::thread::spawn(move || {
            log::info!("[MD5] 开始计算文件MD5: {}", file_path);
            let start_time = std::time::Instant::now();
            
            match md5::calculate_file_md5(&file_path) {
                Ok(actual_md5) => {
                    let elapsed = start_time.elapsed();
                    log::info!("[MD5] 计算完成，耗时: {:?}, 实际MD5: {}", elapsed, actual_md5);
                    
                    if actual_md5 == expected_md5 {
                        log::info!("[MD5] ✓ 校验通过！");
                        let _ = tx.send(Md5VerifyState::Passed);
                    } else {
                        log::error!("[MD5] ✗ 校验失败！预期: {}, 实际: {}", expected_md5, actual_md5);
                        let _ = tx.send(Md5VerifyState::Failed {
                            expected: expected_md5,
                            actual: actual_md5,
                        });
                    }
                }
                Err(e) => {
                    log::error!("[MD5] 计算出错: {}", e);
                    let _ = tx.send(Md5VerifyState::Error(e.to_string()));
                }
            }
        });
    }

    /// 检查MD5校验结果
    fn check_md5_verify_result(&mut self) {
        unsafe {
            if let Some(ref rx) = MD5_VERIFY_RX {
                if let Ok(state) = rx.try_recv() {
                    // 如果校验失败，在状态更新时删除文件（只执行一次）
                    if let Md5VerifyState::Failed { .. } = &state {
                        let filename = self.current_download_filename.clone().unwrap_or_default();
                        let file_path = format!("{}\\{}", self.download_save_path, filename);
                        if let Err(e) = std::fs::remove_file(&file_path) {
                            log::warn!("[MD5] 删除校验失败的文件时出错: {} - {}", file_path, e);
                        } else {
                            log::info!("[MD5] 已删除校验失败的文件: {}", file_path);
                        }
                    }
                    self.md5_verify_state = state;
                    MD5_VERIFY_RX = None;
                }
            }
        }
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
    fn start_download_task_with_pe_check(&mut self, url: &str, save_path: &str, filename: Option<&str>, is_pe_download: bool) {
        self.current_download_filename = filename.map(|s| s.to_string());
        self.current_download = Some(url.to_string());
        self.download_init_error = None;
        self.download_gid = None;
        self.md5_verify_state = Md5VerifyState::NotStarted;  // 重置MD5校验状态

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
        let url = url.to_string();
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
                        status: DownloadStatus::Error(format!("创建运行时失败: {}", e)),
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
                        match crate::download::pe_url_resolver::resolve_pe_download_url(&url_for_resolve).await {
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
                let ((final_url, headers), aria2_result) = tokio::join!(
                    url_resolve_task,
                    aria2_start_task
                );

                let init_elapsed = init_start.elapsed();
                log::info!("[下载] 并行初始化完成，总耗时: {:?}", init_elapsed);

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
                            status: DownloadStatus::Error(format!("初始化aria2失败: {}", e)),
                        });
                        return;
                    }
                };

                // 添加下载任务（根据是否有headers选择方法）
                log::info!("[下载] 准备添加下载任务，检查headers状态...");
                let gid = match headers {
                    Some(hdrs) if !hdrs.is_empty() => {
                        log::info!("[下载] 使用带headers的下载方法，headers数量: {}", hdrs.len());
                        for (i, h) in hdrs.iter().enumerate() {
                            let header_name = h.split(':').next().unwrap_or("Unknown");
                            log::info!("[下载] 传递Header[{}]: {}", i, header_name);
                        }
                        aria2.add_download_with_headers(&final_url, &save_path, filename.as_deref(), Some(hdrs)).await
                    }
                    Some(hdrs) => {
                        log::warn!("[下载] headers为空列表，使用普通下载方法");
                        aria2.add_download(&final_url, &save_path, filename.as_deref()).await
                    }
                    _ => {
                        log::info!("[下载] 无headers，使用普通下载方法");
                        aria2.add_download(&final_url, &save_path, filename.as_deref()).await
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
                            status: DownloadStatus::Error(format!("添加任务失败: {}", e)),
                        });
                        return;
                    }
                };

                // 定期获取进度并发送，同时监听控制命令
                loop {
                    // 处理控制命令（非阻塞）
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
                            let _ = progress_tx.send(DownloadProgress {
                                gid: gid.clone(),
                                completed_length: 0,
                                total_length: 0,
                                download_speed: 0,
                                percentage: 0.0,
                                status: DownloadStatus::Error(format!("获取状态失败: {}", e)),
                            });
                            break;
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
        unsafe {
            DOWNLOAD_CMD_SENDER = Some(_sender);
        }
    }

    fn pause_current_download(&mut self) {
        unsafe {
            if let Some(ref sender) = DOWNLOAD_CMD_SENDER {
                let _ = sender.send(DownloadCommand::Pause);
            }
        }
    }

    fn resume_current_download(&mut self) {
        unsafe {
            if let Some(ref sender) = DOWNLOAD_CMD_SENDER {
                let _ = sender.send(DownloadCommand::Resume);
            }
        }
    }

    fn cancel_current_download(&mut self) {
        unsafe {
            if let Some(ref sender) = DOWNLOAD_CMD_SENDER {
                let _ = sender.send(DownloadCommand::Cancel);
            }
            DOWNLOAD_CMD_SENDER = None;
            MD5_VERIFY_RX = None;
        }

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
        self.md5_verify_state = Md5VerifyState::NotStarted;
        
        unsafe {
            DOWNLOAD_CMD_SENDER = None;
            MD5_VERIFY_RX = None;
        }
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

/// MD5计算模块（纯Rust实现，无外部依赖）
mod md5 {
    use std::io::Read;
    use std::path::Path;
    
    /// MD5上下文
    pub struct Md5Context {
        state: [u32; 4],
        count: [u32; 2],
        buffer: [u8; 64],
    }
    
    impl Md5Context {
        pub fn new() -> Self {
            Self {
                state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
                count: [0, 0],
                buffer: [0u8; 64],
            }
        }
        
        pub fn update(&mut self, input: &[u8]) {
            let input_len = input.len();
            let mut index = ((self.count[0] >> 3) & 0x3F) as usize;
            
            self.count[0] = self.count[0].wrapping_add((input_len as u32) << 3);
            if self.count[0] < (input_len as u32) << 3 {
                self.count[1] = self.count[1].wrapping_add(1);
            }
            self.count[1] = self.count[1].wrapping_add((input_len as u32) >> 29);
            
            let part_len = 64 - index;
            let mut i = 0;
            
            if input_len >= part_len {
                self.buffer[index..64].copy_from_slice(&input[..part_len]);
                let block: [u8; 64] = self.buffer;
                self.transform(&block);
                
                i = part_len;
                while i + 63 < input_len {
                    let block: [u8; 64] = input[i..i + 64].try_into().unwrap();
                    self.transform(&block);
                    i += 64;
                }
                index = 0;
            }
            
            self.buffer[index..index + (input_len - i)].copy_from_slice(&input[i..]);
        }
        
        pub fn finalize(mut self) -> [u8; 16] {
            let bits: [u8; 8] = [
                self.count[0] as u8,
                (self.count[0] >> 8) as u8,
                (self.count[0] >> 16) as u8,
                (self.count[0] >> 24) as u8,
                self.count[1] as u8,
                (self.count[1] >> 8) as u8,
                (self.count[1] >> 16) as u8,
                (self.count[1] >> 24) as u8,
            ];
            
            let index = ((self.count[0] >> 3) & 0x3F) as usize;
            let pad_len = if index < 56 { 56 - index } else { 120 - index };
            
            let mut padding = [0u8; 64];
            padding[0] = 0x80;
            self.update(&padding[..pad_len]);
            self.update(&bits);
            
            let mut digest = [0u8; 16];
            for (i, &s) in self.state.iter().enumerate() {
                digest[i * 4] = s as u8;
                digest[i * 4 + 1] = (s >> 8) as u8;
                digest[i * 4 + 2] = (s >> 16) as u8;
                digest[i * 4 + 3] = (s >> 24) as u8;
            }
            digest
        }
        
        fn transform(&mut self, block: &[u8; 64]) {
            let mut a = self.state[0];
            let mut b = self.state[1];
            let mut c = self.state[2];
            let mut d = self.state[3];
            
            let mut x = [0u32; 16];
            for i in 0..16 {
                x[i] = u32::from_le_bytes([
                    block[i * 4],
                    block[i * 4 + 1],
                    block[i * 4 + 2],
                    block[i * 4 + 3],
                ]);
            }
            
            // Round 1
            macro_rules! ff {
                ($a:expr, $b:expr, $c:expr, $d:expr, $x:expr, $s:expr, $ac:expr) => {
                    $a = $a.wrapping_add(($b & $c) | (!$b & $d))
                        .wrapping_add($x)
                        .wrapping_add($ac);
                    $a = $a.rotate_left($s).wrapping_add($b);
                };
            }
            
            ff!(a, b, c, d, x[0], 7, 0xd76aa478);
            ff!(d, a, b, c, x[1], 12, 0xe8c7b756);
            ff!(c, d, a, b, x[2], 17, 0x242070db);
            ff!(b, c, d, a, x[3], 22, 0xc1bdceee);
            ff!(a, b, c, d, x[4], 7, 0xf57c0faf);
            ff!(d, a, b, c, x[5], 12, 0x4787c62a);
            ff!(c, d, a, b, x[6], 17, 0xa8304613);
            ff!(b, c, d, a, x[7], 22, 0xfd469501);
            ff!(a, b, c, d, x[8], 7, 0x698098d8);
            ff!(d, a, b, c, x[9], 12, 0x8b44f7af);
            ff!(c, d, a, b, x[10], 17, 0xffff5bb1);
            ff!(b, c, d, a, x[11], 22, 0x895cd7be);
            ff!(a, b, c, d, x[12], 7, 0x6b901122);
            ff!(d, a, b, c, x[13], 12, 0xfd987193);
            ff!(c, d, a, b, x[14], 17, 0xa679438e);
            ff!(b, c, d, a, x[15], 22, 0x49b40821);
            
            // Round 2
            macro_rules! gg {
                ($a:expr, $b:expr, $c:expr, $d:expr, $x:expr, $s:expr, $ac:expr) => {
                    $a = $a.wrapping_add(($b & $d) | ($c & !$d))
                        .wrapping_add($x)
                        .wrapping_add($ac);
                    $a = $a.rotate_left($s).wrapping_add($b);
                };
            }
            
            gg!(a, b, c, d, x[1], 5, 0xf61e2562);
            gg!(d, a, b, c, x[6], 9, 0xc040b340);
            gg!(c, d, a, b, x[11], 14, 0x265e5a51);
            gg!(b, c, d, a, x[0], 20, 0xe9b6c7aa);
            gg!(a, b, c, d, x[5], 5, 0xd62f105d);
            gg!(d, a, b, c, x[10], 9, 0x02441453);
            gg!(c, d, a, b, x[15], 14, 0xd8a1e681);
            gg!(b, c, d, a, x[4], 20, 0xe7d3fbc8);
            gg!(a, b, c, d, x[9], 5, 0x21e1cde6);
            gg!(d, a, b, c, x[14], 9, 0xc33707d6);
            gg!(c, d, a, b, x[3], 14, 0xf4d50d87);
            gg!(b, c, d, a, x[8], 20, 0x455a14ed);
            gg!(a, b, c, d, x[13], 5, 0xa9e3e905);
            gg!(d, a, b, c, x[2], 9, 0xfcefa3f8);
            gg!(c, d, a, b, x[7], 14, 0x676f02d9);
            gg!(b, c, d, a, x[12], 20, 0x8d2a4c8a);
            
            // Round 3
            macro_rules! hh {
                ($a:expr, $b:expr, $c:expr, $d:expr, $x:expr, $s:expr, $ac:expr) => {
                    $a = $a.wrapping_add($b ^ $c ^ $d)
                        .wrapping_add($x)
                        .wrapping_add($ac);
                    $a = $a.rotate_left($s).wrapping_add($b);
                };
            }
            
            hh!(a, b, c, d, x[5], 4, 0xfffa3942);
            hh!(d, a, b, c, x[8], 11, 0x8771f681);
            hh!(c, d, a, b, x[11], 16, 0x6d9d6122);
            hh!(b, c, d, a, x[14], 23, 0xfde5380c);
            hh!(a, b, c, d, x[1], 4, 0xa4beea44);
            hh!(d, a, b, c, x[4], 11, 0x4bdecfa9);
            hh!(c, d, a, b, x[7], 16, 0xf6bb4b60);
            hh!(b, c, d, a, x[10], 23, 0xbebfbc70);
            hh!(a, b, c, d, x[13], 4, 0x289b7ec6);
            hh!(d, a, b, c, x[0], 11, 0xeaa127fa);
            hh!(c, d, a, b, x[3], 16, 0xd4ef3085);
            hh!(b, c, d, a, x[6], 23, 0x04881d05);
            hh!(a, b, c, d, x[9], 4, 0xd9d4d039);
            hh!(d, a, b, c, x[12], 11, 0xe6db99e5);
            hh!(c, d, a, b, x[15], 16, 0x1fa27cf8);
            hh!(b, c, d, a, x[2], 23, 0xc4ac5665);
            
            // Round 4
            macro_rules! ii {
                ($a:expr, $b:expr, $c:expr, $d:expr, $x:expr, $s:expr, $ac:expr) => {
                    $a = $a.wrapping_add($c ^ ($b | !$d))
                        .wrapping_add($x)
                        .wrapping_add($ac);
                    $a = $a.rotate_left($s).wrapping_add($b);
                };
            }
            
            ii!(a, b, c, d, x[0], 6, 0xf4292244);
            ii!(d, a, b, c, x[7], 10, 0x432aff97);
            ii!(c, d, a, b, x[14], 15, 0xab9423a7);
            ii!(b, c, d, a, x[5], 21, 0xfc93a039);
            ii!(a, b, c, d, x[12], 6, 0x655b59c3);
            ii!(d, a, b, c, x[3], 10, 0x8f0ccc92);
            ii!(c, d, a, b, x[10], 15, 0xffeff47d);
            ii!(b, c, d, a, x[1], 21, 0x85845dd1);
            ii!(a, b, c, d, x[8], 6, 0x6fa87e4f);
            ii!(d, a, b, c, x[15], 10, 0xfe2ce6e0);
            ii!(c, d, a, b, x[6], 15, 0xa3014314);
            ii!(b, c, d, a, x[13], 21, 0x4e0811a1);
            ii!(a, b, c, d, x[4], 6, 0xf7537e82);
            ii!(d, a, b, c, x[11], 10, 0xbd3af235);
            ii!(c, d, a, b, x[2], 15, 0x2ad7d2bb);
            ii!(b, c, d, a, x[9], 21, 0xeb86d391);
            
            self.state[0] = self.state[0].wrapping_add(a);
            self.state[1] = self.state[1].wrapping_add(b);
            self.state[2] = self.state[2].wrapping_add(c);
            self.state[3] = self.state[3].wrapping_add(d);
        }
    }
    
    /// 计算文件的MD5值
    pub fn calculate_file_md5<P: AsRef<Path>>(path: P) -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let mut context = Md5Context::new();
        let mut buffer = [0u8; 65536];  // 64KB缓冲区，提高大文件读取速度
        
        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            context.update(&buffer[..bytes_read]);
        }
        
        let digest = context.finalize();
        Ok(digest.iter().map(|b| format!("{:02X}", b)).collect())
    }
}
