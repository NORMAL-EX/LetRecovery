use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;

use crate::core::config::{ConfigFileManager, OperationType};
use crate::core::dism::DismProgress;
use crate::ui::progress::{InstallStep, BackupStep, ProgressState, ProgressUI};
use crate::utils::reboot_pe;

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
pub enum WorkerMessage {
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

pub struct App {
    /// 进度状态
    progress_state: Arc<Mutex<ProgressState>>,
    /// 消息接收器
    message_rx: Option<Receiver<WorkerMessage>>,
    /// 是否已启动
    started: bool,
    /// 操作类型
    operation_type: Option<OperationType>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 设置中文字体
        Self::setup_fonts(&cc.egui_ctx);

        // 检测操作类型
        let operation_type = ConfigFileManager::detect_operation_type();

        let progress_state = Arc::new(Mutex::new(match operation_type {
            Some(OperationType::Install) => ProgressState::new_install(),
            Some(OperationType::Backup) => ProgressState::new_backup(),
            None => ProgressState::new_install(),
        }));

        Self {
            progress_state,
            message_rx: None,
            started: false,
            operation_type,
        }
    }

    /// 设置中文字体（从PE的X盘加载微软雅黑）
    fn setup_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();

        // PE环境下字体路径固定为 X:\Windows\Fonts\msyh.ttc
        let font_path = std::path::Path::new("X:\\Windows\\Fonts\\msyh.ttc");

        if let Ok(font_data) = std::fs::read(font_path) {
            fonts.font_data.insert(
                "msyh".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(font_data)),
            );

            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "msyh".to_owned());

            fonts
                .families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .insert(0, "msyh".to_owned());

            log::info!("已加载中文字体: X:\\Windows\\Fonts\\msyh.ttc");
        } else {
            log::warn!("无法加载中文字体: X:\\Windows\\Fonts\\msyh.ttc");
        }

        ctx.set_fonts(fonts);
    }

    /// 启动工作线程
    fn start_worker(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        let (tx, rx) = channel::<WorkerMessage>();
        self.message_rx = Some(rx);

        let operation_type = self.operation_type;

        thread::spawn(move || {
            match operation_type {
                Some(OperationType::Install) => {
                    execute_install_workflow(tx);
                }
                Some(OperationType::Backup) => {
                    execute_backup_workflow(tx);
                }
                None => {
                    let _ = tx.send(WorkerMessage::Failed("未检测到安装或备份配置".to_string()));
                }
            }
        });
    }

    /// 处理工作线程消息
    fn process_messages(&mut self) {
        if let Some(ref rx) = self.message_rx {
            while let Ok(msg) = rx.try_recv() {
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
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 启动工作线程
        if !self.started {
            self.start_worker();
        }

        // 处理消息
        self.process_messages();

        // 绘制界面
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Ok(state) = self.progress_state.lock() {
                ProgressUI::show(ui, &state);
            }
        });

        // 持续刷新
        ctx.request_repaint();
    }
}

/// 执行安装工作流
fn execute_install_workflow(tx: Sender<WorkerMessage>) {
    use crate::core::bcdedit::BootManager;
    use crate::core::dism::Dism;
    use crate::core::disk::DiskManager;
    use crate::core::ghost::Ghost;
    use crate::ui::advanced_options::apply_advanced_options;

    log::info!("========== 开始PE安装流程 ==========");

    // 查找配置文件所在分区
    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(p) => p,
        None => {
            let _ = tx.send(WorkerMessage::Failed("未找到安装配置文件".to_string()));
            return;
        }
    };

    log::info!("数据分区: {}", data_partition);
    let _ = tx.send(WorkerMessage::SetStatus(format!("数据分区: {}", data_partition)));

    // 读取安装配置
    let config = match ConfigFileManager::read_install_config(&data_partition) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(WorkerMessage::Failed(format!("读取配置失败: {}", e)));
            return;
        }
    };

    log::info!("目标分区: {}", config.target_partition);
    log::info!("镜像文件: {}", config.image_path);

    // 查找安装标记分区
    let target_partition = ConfigFileManager::find_install_marker_partition()
        .unwrap_or_else(|| config.target_partition.clone());

    // 构建完整镜像路径
    let data_dir = ConfigFileManager::get_data_dir(&data_partition);
    let image_path = format!("{}\\{}", data_dir, config.image_path);

    if !std::path::Path::new(&image_path).exists() {
        let _ = tx.send(WorkerMessage::Failed(format!("镜像文件不存在: {}", image_path)));
        return;
    }

    log::info!("完整镜像路径: {}", image_path);

    // Step 1: 格式化分区
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::FormatPartition));
    let _ = tx.send(WorkerMessage::SetStatus("正在格式化目标分区...".to_string()));

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
            let _ = tx.send(WorkerMessage::Failed(format!("格式化分区失败: {}", e)));
            return;
        }
    }

    // Step 2: 释放镜像
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::ApplyImage));
    let _ = tx.send(WorkerMessage::SetStatus("正在释放系统镜像...".to_string()));

    let apply_dir = format!("{}\\", target_partition);

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
            let _ = tx.send(WorkerMessage::Failed("Ghost工具不可用".to_string()));
            return;
        }

        let partitions = DiskManager::get_partitions().unwrap_or_default();
        ghost.restore_image_to_letter(&image_path, &target_partition, &partitions, Some(progress_tx))
    } else {
        // WIM/ESD使用DISM
        let dism = Dism::new();
        dism.apply_image(&image_path, &apply_dir, config.volume_index, Some(progress_tx))
    };

    // 等待进度监控线程结束
    let _ = progress_handle.join();

    if let Err(e) = apply_result {
        let _ = tx.send(WorkerMessage::Failed(format!("释放镜像失败: {}", e)));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 3: 导入驱动
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::ImportDrivers));

    // 根据 driver_action_mode 决定是否导入驱动
    // 0 = 无, 1 = 仅保存（不导入）, 2 = 自动导入
    let driver_path = format!("{}\\drivers", data_dir);
    let driver_path_exists = std::path::Path::new(&driver_path).exists();
    
    if config.should_import_drivers() && driver_path_exists {
        let _ = tx.send(WorkerMessage::SetStatus("正在导入驱动...".to_string()));
        
        // 创建进度通道
        let (driver_progress_tx, driver_progress_rx) = channel::<DismProgress>();
        let tx_driver = tx.clone();
        
        // 启动进度监控线程
        let driver_progress_handle = thread::spawn(move || {
            while let Ok(progress) = driver_progress_rx.recv() {
                let _ = tx_driver.send(WorkerMessage::SetProgress(progress.percentage));
                let _ = tx_driver.send(WorkerMessage::SetStatus(format!("导入驱动: {}", progress.status)));
            }
        });
        
        let dism = Dism::new();
        match dism.add_drivers_offline_with_progress(&apply_dir, &driver_path, Some(driver_progress_tx)) {
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
            log::info!("在驱动目录中发现 {} 个 CAB 文件，将一并安装", cab_files_in_driver_dir.len());
            let _ = tx.send(WorkerMessage::SetStatus(format!(
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
                    let _ = tx_cab.send(WorkerMessage::SetStatus(format!("安装CAB: {}", progress.status)));
                }
            });
            
            let dism = Dism::new();
            match dism.add_packages_offline_from_dir(&apply_dir, &driver_path, Some(cab_progress_tx)) {
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
        let _ = tx.send(WorkerMessage::SetStatus("跳过驱动导入（目录不存在）".to_string()));
    } else if config.has_driver_data() {
        // SaveOnly 模式：驱动已保存但不导入
        let _ = tx.send(WorkerMessage::SetStatus("跳过驱动导入（仅保存模式）".to_string()));
        log::info!("驱动操作模式为仅保存，跳过驱动导入");
    } else {
        let _ = tx.send(WorkerMessage::SetStatus("跳过驱动导入".to_string()));
        log::info!("驱动操作模式为无，跳过驱动导入");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 4: 安装CAB更新包
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::InstallCabPackages));

    if config.install_cab_packages {
        let cab_path = format!("{}\\updates", data_dir);
        if std::path::Path::new(&cab_path).exists() {
            let _ = tx.send(WorkerMessage::SetStatus("正在安装更新包...".to_string()));
            
            // 创建进度通道
            let (cab_progress_tx, cab_progress_rx) = channel::<DismProgress>();
            let tx_cab = tx.clone();
            
            // 启动进度监控线程
            let cab_progress_handle = thread::spawn(move || {
                while let Ok(progress) = cab_progress_rx.recv() {
                    let _ = tx_cab.send(WorkerMessage::SetProgress(progress.percentage));
                    let _ = tx_cab.send(WorkerMessage::SetStatus(format!("安装更新: {}", progress.status)));
                }
            });
            
            let dism = Dism::new();
            match dism.add_packages_offline_from_dir(&apply_dir, &cab_path, Some(cab_progress_tx)) {
                Ok((success, fail)) => {
                    log::info!("CAB更新包安装完成: {} 成功, {} 失败", success, fail);
                    let _ = tx.send(WorkerMessage::SetStatus(
                        format!("更新包安装完成: {} 成功, {} 失败", success, fail)
                    ));
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
            let _ = tx.send(WorkerMessage::SetStatus("跳过更新包安装（目录不存在）".to_string()));
        }
    } else {
        let _ = tx.send(WorkerMessage::SetStatus("跳过更新包安装".to_string()));
        log::info!("未启用CAB更新包安装");
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 5: 修复引导
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::RepairBoot));
    let _ = tx.send(WorkerMessage::SetStatus("正在修复引导...".to_string()));

    let boot_manager = BootManager::new();
    let use_uefi = DiskManager::detect_uefi_mode();

    if let Err(e) = boot_manager.repair_boot_advanced(&target_partition, use_uefi) {
        let _ = tx.send(WorkerMessage::Failed(format!("修复引导失败: {}", e)));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 6: 应用高级选项
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::ApplyAdvancedOptions));
    let _ = tx.send(WorkerMessage::SetStatus("正在应用高级选项...".to_string()));

    if let Err(e) = apply_advanced_options(&target_partition, &config) {
        log::warn!("应用高级选项失败: {}", e);
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 7: 生成无人值守配置
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::GenerateUnattend));

    if config.unattended {
        let _ = tx.send(WorkerMessage::SetStatus("正在生成无人值守配置...".to_string()));
        if let Err(e) = generate_unattend_xml(&target_partition, &config) {
            log::warn!("生成无人值守配置失败: {}", e);
        }
    } else {
        let _ = tx.send(WorkerMessage::SetStatus("跳过无人值守配置".to_string()));
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 8: 清理临时文件
    let _ = tx.send(WorkerMessage::SetInstallStep(InstallStep::Cleanup));
    let _ = tx.send(WorkerMessage::SetStatus("正在清理临时文件...".to_string()));

    ConfigFileManager::cleanup_all(&data_partition, &target_partition);
    let _ = tx.send(WorkerMessage::SetProgress(50));

    // 清理自动创建的数据分区并扩展目标分区
    let _ = tx.send(WorkerMessage::SetStatus("正在清理自动创建的分区...".to_string()));
    match DiskManager::cleanup_auto_created_partition_and_extend(&target_partition) {
        Ok(_) => {
            log::info!("自动创建分区清理完成");
        }
        Err(e) => {
            // 不中断安装流程，只记录警告
            log::warn!("清理自动创建分区失败: {}", e);
        }
    }
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

/// 执行备份工作流
fn execute_backup_workflow(tx: Sender<WorkerMessage>) {
    use crate::core::bcdedit::BootManager;
    use crate::core::config::BackupFormat;
    use crate::core::dism::Dism;
    use crate::core::ghost::Ghost;

    log::info!("========== 开始PE备份流程 ==========");

    // 查找配置文件所在分区
    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(p) => p,
        None => {
            let _ = tx.send(WorkerMessage::Failed("未找到备份配置文件".to_string()));
            return;
        }
    };

    log::info!("数据分区: {}", data_partition);

    // Step 1: 读取配置
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::ReadConfig));
    let _ = tx.send(WorkerMessage::SetStatus("正在读取备份配置...".to_string()));

    let config = match ConfigFileManager::read_backup_config(&data_partition) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(WorkerMessage::Failed(format!("读取配置失败: {}", e)));
            return;
        }
    };

    log::info!("源分区: {}", config.source_partition);
    log::info!("保存路径: {}", config.save_path);
    log::info!("备份格式: {:?}", config.format);
    if config.format == BackupFormat::Swm {
        log::info!("SWM分卷大小: {} MB", config.swm_split_size);
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // 查找备份标记分区
    let source_partition = ConfigFileManager::find_backup_marker_partition()
        .unwrap_or_else(|| config.source_partition.clone());

    // Step 2: 执行备份
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::CaptureImage));
    
    let capture_dir = format!("{}\\", source_partition);

    // 创建进度通道
    let (progress_tx, progress_rx) = channel::<DismProgress>();
    let tx_clone = tx.clone();

    let progress_handle = thread::spawn(move || {
        while let Ok(progress) = progress_rx.recv() {
            let _ = tx_clone.send(WorkerMessage::SetProgress(progress.percentage));
        }
    });

    let backup_result = match config.format {
        BackupFormat::Gho => {
            // GHO格式使用Ghost
            let _ = tx.send(WorkerMessage::SetStatus("正在使用Ghost备份系统...".to_string()));
            let ghost = Ghost::new();
            if !ghost.is_available() {
                drop(progress_handle);
                let _ = tx.send(WorkerMessage::Failed("Ghost工具不可用".to_string()));
                return;
            }
            
            // Ghost备份
            ghost.create_image_from_letter(&source_partition, &config.save_path, Some(progress_tx))
        }
        BackupFormat::Esd => {
            // ESD格式使用DISM高压缩
            let _ = tx.send(WorkerMessage::SetStatus("正在备份系统（ESD高压缩）...".to_string()));
            let dism = Dism::new();
            if config.incremental && std::path::Path::new(&config.save_path).exists() {
                dism.append_image_esd(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            } else {
                dism.capture_image_esd(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            }
        }
        BackupFormat::Swm => {
            // SWM分卷格式
            let _ = tx.send(WorkerMessage::SetStatus(format!("正在备份系统（SWM分卷，每卷{}MB）...", config.swm_split_size).to_string()));
            let dism = Dism::new();
            dism.capture_image_swm(
                &config.save_path,
                &capture_dir,
                &config.name,
                &config.description,
                config.swm_split_size,
                Some(progress_tx),
            )
        }
        BackupFormat::Wim => {
            // 标准WIM格式
            let _ = tx.send(WorkerMessage::SetStatus("正在执行系统备份...".to_string()));
            let dism = Dism::new();
            if config.incremental && std::path::Path::new(&config.save_path).exists() {
                dism.append_image(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            } else {
                dism.capture_image(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            }
        }
    };

    // 等待进度监控线程结束
    let _ = progress_handle.join();

    if let Err(e) = backup_result {
        let _ = tx.send(WorkerMessage::Failed(format!("备份失败: {}", e)));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 3: 验证备份文件
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::VerifyBackup));
    let _ = tx.send(WorkerMessage::SetStatus("正在验证备份文件...".to_string()));

    // 对于SWM格式，检查第一个分卷文件
    let verify_path = if config.format == BackupFormat::Swm {
        // SWM的第一个文件可能是 xxx.swm 或 xxx.swm
        config.save_path.clone()
    } else {
        config.save_path.clone()
    };
    
    if !std::path::Path::new(&verify_path).exists() {
        let _ = tx.send(WorkerMessage::Failed("备份文件验证失败".to_string()));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 4: 恢复引导
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::RepairBoot));
    let _ = tx.send(WorkerMessage::SetStatus("正在恢复引导...".to_string()));

    let boot_manager = BootManager::new();
    // 删除当前PE引导项
    let _ = boot_manager.delete_current_boot_entry();
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // Step 5: 清理
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::Cleanup));
    let _ = tx.send(WorkerMessage::SetStatus("正在清理临时文件...".to_string()));

    ConfigFileManager::cleanup_partition_markers(&source_partition);
    ConfigFileManager::cleanup_data_dir(&data_partition);
    ConfigFileManager::cleanup_pe_dir(&data_partition);
    let _ = tx.send(WorkerMessage::SetProgress(100));

    // 完成
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::Complete));
    let _ = tx.send(WorkerMessage::Completed);

    log::info!("========== PE备份流程完成 ==========");

    // 自动重启
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
fn generate_unattend_xml(target_partition: &str, config: &crate::core::config::InstallConfig) -> anyhow::Result<()> {
    use crate::ui::advanced_options::get_scripts_dir_name;
    use crate::core::system_utils::{get_file_version, get_offline_system_architecture};
    use std::path::Path;
    
    let username = if config.custom_username.is_empty() { 
        "User".to_string() 
    } else { 
        config.custom_username.clone() 
    };

    let scripts_dir = get_scripts_dir_name();

    // 检测目标系统架构
    let arch = get_offline_system_architecture(Path::new(target_partition));
    let arch_str = arch.as_unattend_str();
    log::info!("[UNATTEND] 检测到目标系统架构: {}", arch_str);

    // 通过 ntdll.dll 文件版本检测目标系统版本
    // Windows 7: 6.1.x, Windows 8: 6.2.x, Windows 8.1: 6.3.x, Windows 10/11: 10.0.x
    let ntdll_path = Path::new(target_partition).join("Windows").join("System32").join("ntdll.dll");
    let (is_win7, is_win8) = match get_file_version(&ntdll_path) {
        Some((major, minor, build, _)) => {
            log::info!("[UNATTEND] 检测到目标系统版本 (ntdll.dll): {}.{}.{}", major, minor, build);
            
            let is_win7 = major == 6 && minor == 1;
            let is_win8 = major == 6 && (minor == 2 || minor == 3);
            (is_win7, is_win8)
        }
        None => {
            log::warn!("[UNATTEND] 无法读取 ntdll.dll 版本: {:?}, 默认使用 Win10/11 配置", ntdll_path);
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
    first_logon_commands.push_str(&format!(r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>cmd /c rd /s /q %SystemDrive%\{}</CommandLine>
                    <Description>Cleanup scripts directory</Description>
                </SynchronousCommand>"#, order, scripts_dir));

    // 根据系统版本生成不同的 XML 内容
    let xml_content = if is_win7 {
        // Windows 7 专用无人值守配置
        // Win7 不支持: HideOnlineAccountScreens, HideWirelessSetupInOOBE, SkipMachineOOBE, SkipUserOOBE, HideLocalAccountScreen, HideOEMRegistrationScreen(家庭版)
        generate_win7_unattend_xml(&username, &scripts_dir, &first_logon_commands, arch_str)
    } else if is_win8 {
        // Windows 8/8.1 无人值守配置
        // Win8 支持部分 Win10 的选项，但不支持所有
        generate_win8_unattend_xml(&username, &scripts_dir, &first_logon_commands, arch_str)
    } else {
        // Windows 10/11 无人值守配置（默认）
        generate_win10_unattend_xml(&username, &scripts_dir, &first_logon_commands, arch_str)
    };

    let panther_dir = format!("{}\\Windows\\Panther", target_partition);
    std::fs::create_dir_all(&panther_dir)?;

    let unattend_path = format!("{}\\unattend.xml", panther_dir);
    std::fs::write(&unattend_path, &xml_content)?;
    log::info!("[UNATTEND] 已写入: {} ({})", unattend_path, 
        if is_win7 { "Win7配置" } else if is_win8 { "Win8配置" } else { "Win10/11配置" });

    // 同时写入到 Sysprep 目录
    let sysprep_dir = format!("{}\\Windows\\System32\\Sysprep", target_partition);
    if std::path::Path::new(&sysprep_dir).exists() {
        let sysprep_unattend = format!("{}\\unattend.xml", sysprep_dir);
        let _ = std::fs::write(&sysprep_unattend, &xml_content);
        log::info!("[UNATTEND] 已写入: {}", sysprep_unattend);
    }

    Ok(())
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
fn generate_win7_unattend_xml(username: &str, scripts_dir: &str, first_logon_commands: &str, arch: &str) -> String {
    // Win7 使用最小化的OOBE配置以确保兼容所有版本（包括家庭版）
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
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
</unattend>"#, arch = arch, scripts_dir = scripts_dir, username = username, first_logon_commands = first_logon_commands)
}

/// 生成 Windows 8/8.1 专用的无人值守配置
/// 
/// Win8/8.1 支持部分 Win10 的选项：
/// - 支持 HideLocalAccountScreen
/// - 不支持 HideOnlineAccountScreens
/// - 不支持 HideWirelessSetupInOOBE
/// - 不支持 SkipMachineOOBE / SkipUserOOBE
fn generate_win8_unattend_xml(username: &str, scripts_dir: &str, first_logon_commands: &str, arch: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
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
</unattend>"#, arch = arch, scripts_dir = scripts_dir, username = username, first_logon_commands = first_logon_commands)
}

/// 生成 Windows 10/11 无人值守配置
/// 
/// 完整支持所有 OOBE 跳过选项：
/// - HideLocalAccountScreen
/// - HideOnlineAccountScreens
/// - HideWirelessSetupInOOBE
/// - SkipMachineOOBE
/// - SkipUserOOBE
fn generate_win10_unattend_xml(username: &str, scripts_dir: &str, first_logon_commands: &str, arch: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
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
                <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
                <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
                <ProtectYourPC>3</ProtectYourPC>
                <SkipMachineOOBE>true</SkipMachineOOBE>
                <SkipUserOOBE>true</SkipUserOOBE>
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
</unattend>"#, arch = arch, scripts_dir = scripts_dir, username = username, first_logon_commands = first_logon_commands)
}
