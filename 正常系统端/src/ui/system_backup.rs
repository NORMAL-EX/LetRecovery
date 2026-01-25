use egui;
use std::sync::mpsc;
use std::path::Path;

use crate::app::{App, BackupFormat, BackupMode, Panel};
use crate::core::dism::{Dism, DismProgress};
use crate::core::install_config::{BackupConfig, ConfigFileManager};

impl App {
    pub fn show_system_backup(&mut self, ui: &mut egui::Ui) {
        ui.heading("系统备份");
        ui.separator();

        let is_pe = self.is_pe_environment();
        
        // 判断是否需要通过PE备份
        let needs_pe = self.check_if_needs_pe_for_backup();
        
        // 检查PE配置是否可用
        let pe_available = self.is_pe_config_available();
        
        // 在非PE环境且源是系统分区时，需要显示PE选择
        let show_pe_selector = !is_pe && needs_pe;
        
        // 备份按钮是否可用
        let backup_blocked = show_pe_selector && !pe_available;

        // 选择要备份的分区
        ui.label("选择要备份的分区:");

        egui::ScrollArea::vertical()
            .max_height(150.0)
            .show(ui, |ui| {
                egui::Grid::new("backup_partition_grid")
                    .striped(true)
                    .min_col_width(80.0)
                    .show(ui, |ui| {
                        ui.label("分区卷");
                        ui.label("总空间");
                        ui.label("已用空间");
                        ui.label("卷标");
                        ui.label("状态");
                        ui.end_row();

                        for (i, partition) in self.partitions.iter().enumerate() {
                            let used_size = partition.total_size_mb - partition.free_size_mb;
                            
                            let label = if is_pe {
                                if partition.has_windows {
                                    format!("{} (有系统)", partition.letter)
                                } else {
                                    partition.letter.clone()
                                }
                            } else {
                                if partition.is_system_partition {
                                    format!("{} (当前系统)", partition.letter)
                                } else if partition.has_windows {
                                    format!("{} (有系统)", partition.letter)
                                } else {
                                    partition.letter.clone()
                                }
                            };

                            if ui
                                .selectable_label(self.backup_source_partition == Some(i), &label)
                                .clicked()
                            {
                                self.backup_source_partition = Some(i);
                            }

                            ui.label(Self::format_size(partition.total_size_mb));
                            ui.label(Self::format_size(used_size));
                            ui.label(&partition.label);
                            
                            let status = if partition.has_windows {
                                "有系统"
                            } else {
                                "无系统"
                            };
                            ui.label(status);
                            
                            ui.end_row();
                        }
                    });
            });

        ui.add_space(15.0);
        ui.separator();

        // 备份格式选择
        ui.horizontal(|ui| {
            ui.label("备份格式:");
            egui::ComboBox::from_id_salt("backup_format_select")
                .selected_text(format!("{}", self.backup_format))
                .width(80.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.backup_format,
                        BackupFormat::Wim,
                        "WIM (推荐)",
                    );
                    ui.selectable_value(
                        &mut self.backup_format,
                        BackupFormat::Esd,
                        "ESD (高压缩)",
                    );
                    ui.selectable_value(
                        &mut self.backup_format,
                        BackupFormat::Swm,
                        "SWM (分卷)",
                    );
                    ui.selectable_value(
                        &mut self.backup_format,
                        BackupFormat::Gho,
                        "GHO (Ghost)",
                    );
                });
            
            // 显示格式说明
            match self.backup_format {
                BackupFormat::Wim => {
                    ui.label("标准WIM格式，兼容性好");
                }
                BackupFormat::Esd => {
                    ui.label("高压缩率，体积更小");
                }
                BackupFormat::Swm => {
                    ui.label("分卷存储，便于传输");
                }
                BackupFormat::Gho => {
                    ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "需要Ghost工具支持");
                }
            }
        });

        // SWM分卷大小设置
        if self.backup_format == BackupFormat::Swm {
            ui.horizontal(|ui| {
                ui.label("分卷大小:");
                ui.add(egui::DragValue::new(&mut self.backup_swm_split_size)
                    .range(512..=8192)
                    .speed(100)
                    .suffix(" MB"));
                ui.label("(512-8192 MB)");
            });
        }

        ui.add_space(10.0);

        // 备份保存位置
        ui.horizontal(|ui| {
            ui.label("保存位置:");
            ui.add(
                egui::TextEdit::singleline(&mut self.backup_save_path).desired_width(400.0),
            );
            if ui.button("浏览...").clicked() {
                let ext = self.backup_format.extension();
                let desc = self.backup_format.filter_description();
                let default_name = format!("backup.{}", ext);
                
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(desc, &[ext])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    self.backup_save_path = path.to_string_lossy().to_string();
                    // 如果保存位置的文件存在，自动勾选增量备份；否则取消勾选
                    self.backup_incremental = Path::new(&self.backup_save_path).exists();
                }
            }
        });

        // 备份名称
        ui.horizontal(|ui| {
            ui.label("备份名称:");
            ui.add(
                egui::TextEdit::singleline(&mut self.backup_name).desired_width(300.0),
            );
        });

        // 备份描述
        ui.horizontal(|ui| {
            ui.label("备份描述:");
            ui.add(
                egui::TextEdit::singleline(&mut self.backup_description).desired_width(300.0),
            );
        });

        ui.add_space(15.0);

        // 备份选项
        ui.checkbox(&mut self.backup_incremental, "增量备份 (追加到现有镜像)");

        // PE选择（仅在需要通过PE备份时显示）
        if show_pe_selector {
            ui.add_space(10.0);
            ui.separator();
            
            ui.horizontal(|ui| {
                ui.label("🔧 PE环境:");
                
                if pe_available {
                    if let Some(ref config) = self.config {
                        egui::ComboBox::from_id_salt("pe_select_backup")
                            .selected_text(
                                self.selected_pe_for_backup
                                    .and_then(|i| config.pe_list.get(i))
                                    .map(|p| p.display_name.as_str())
                                    .unwrap_or("请选择PE"),
                            )
                            .show_ui(ui, |ui| {
                                for (i, pe) in config.pe_list.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.selected_pe_for_backup,
                                        Some(i),
                                        &pe.display_name,
                                    );
                                }
                            });
                        
                        // 显示PE就绪状态
                        if let Some(idx) = self.selected_pe_for_backup {
                            if let Some(pe) = config.pe_list.get(idx) {
                                let (exists, _) = crate::core::pe::PeManager::check_pe_exists(&pe.filename);
                                if exists {
                                    ui.colored_label(egui::Color32::GREEN, "✓ 已就绪");
                                } else {
                                    ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "需下载");
                                }
                            }
                        }
                    }
                } else {
                    ui.colored_label(egui::Color32::RED, "未找到PE配置");
                }
            });
            
            ui.colored_label(
                egui::Color32::from_rgb(255, 165, 0),
                "⚠ 备份当前系统分区需要先重启到PE环境",
            );
        }

        // PE配置缺失警告
        if backup_blocked {
            ui.add_space(5.0);
            ui.colored_label(
                egui::Color32::RED,
                "❌ 无法获取PE配置，无法备份当前系统分区。请检查网络连接后重试。",
            );
        }

        ui.add_space(20.0);

        // 开始备份按钮
        let can_backup = self.backup_source_partition.is_some()
            && !self.backup_save_path.is_empty()
            && !self.backup_name.is_empty()
            && !backup_blocked
            && (!show_pe_selector || self.selected_pe_for_backup.is_some());

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_backup && !self.is_backing_up,
                    egui::Button::new("开始备份").min_size(egui::vec2(120.0, 35.0)),
                )
                .clicked()
            {
                self.start_backup();
            }

            // 显示备份模式提示
            if can_backup {
                if needs_pe && !is_pe {
                    ui.label("(将通过PE环境备份)");
                } else {
                    ui.label("(直接备份)");
                }
            }
        });

        // 备份进度
        if self.is_backing_up {
            self.update_backup_progress();
            
            ui.add_space(15.0);
            ui.separator();
            ui.label(format!("备份进度: {}%", self.backup_progress));
            ui.add(
                egui::ProgressBar::new(self.backup_progress as f32 / 100.0)
                    .show_percentage()
                    .animate(true),
            );
        }

        // 显示备份完成
        if self.backup_progress >= 100 && !self.is_backing_up {
            ui.add_space(10.0);
            ui.colored_label(egui::Color32::GREEN, "✓ 备份完成！");
        }

        // 显示备份错误
        if let Some(ref error) = self.backup_error {
            ui.add_space(10.0);
            ui.colored_label(egui::Color32::RED, format!("✗ {}", error));
        }

        // 状态提示
        if !can_backup && !self.is_backing_up {
            ui.add_space(10.0);
            if self.backup_source_partition.is_none() {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "请选择要备份的分区");
            } else if self.backup_save_path.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "请选择保存位置");
            } else if self.backup_name.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "请输入备份名称");
            }
        }

        // 警告：备份没有系统的分区
        if let Some(idx) = self.backup_source_partition {
            if let Some(partition) = self.partitions.get(idx) {
                if !partition.has_windows {
                    ui.add_space(5.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "⚠ 所选分区似乎没有 Windows 系统",
                    );
                }
            }
        }
    }

    /// 检查是否需要通过PE备份
    fn check_if_needs_pe_for_backup(&self) -> bool {
        // 如果已经在PE环境中，不需要再进PE
        if self.is_pe_environment() {
            return false;
        }
        
        // 检查源分区是否是当前系统分区
        if let Some(idx) = self.backup_source_partition {
            if let Some(partition) = self.partitions.get(idx) {
                return partition.is_system_partition;
            }
        }
        
        false
    }

    fn start_backup(&mut self) {
        let source_partition = self
            .partitions
            .get(self.backup_source_partition.unwrap())
            .cloned();
        if source_partition.is_none() {
            return;
        }
        let source_partition = source_partition.unwrap();

        let is_system_partition = source_partition.is_system_partition;
        let is_pe = self.is_pe_environment();

        // 确定备份模式
        self.backup_mode = if is_pe || !is_system_partition {
            BackupMode::Direct
        } else {
            BackupMode::ViaPE
        };

        // 如果需要通过PE备份，先检查PE是否存在
        if self.backup_mode == BackupMode::ViaPE {
            let pe_info = self.selected_pe_for_backup.and_then(|idx| {
                self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
            });
            
            if let Some(pe) = pe_info {
                let (pe_exists, _) = crate::core::pe::PeManager::check_pe_exists(&pe.filename);
                if !pe_exists {
                    // PE不存在，先下载PE
                    println!("[BACKUP] PE文件不存在，开始下载: {}", pe.filename);
                    self.pending_download_url = Some(pe.download_url.clone());
                    self.pending_download_filename = Some(pe.filename.clone());
                    self.pending_pe_md5 = pe.md5.clone();  // 设置MD5校验值
                    let pe_dir = crate::utils::path::get_exe_dir()
                        .join("PE")
                        .to_string_lossy()
                        .to_string();
                    self.download_save_path = pe_dir;
                    self.pe_download_then_action = Some(crate::app::PeDownloadThenAction::Backup);
                    self.current_panel = crate::app::Panel::DownloadProgress;
                    return;
                }
            }
        }

        // 执行实际的备份
        self.start_backup_internal();
    }
    
    /// 内部备份函数，PE下载完成后调用
    pub fn start_backup_internal(&mut self) {
        let source_partition = self
            .partitions
            .get(self.backup_source_partition.unwrap())
            .cloned();
        if source_partition.is_none() {
            return;
        }
        let source_partition = source_partition.unwrap();

        let is_system_partition = source_partition.is_system_partition;
        let is_pe = self.is_pe_environment();

        // 确定备份模式
        self.backup_mode = if is_pe || !is_system_partition {
            BackupMode::Direct
        } else {
            BackupMode::ViaPE
        };

        self.is_backing_up = true;
        self.backup_progress = 0;
        self.backup_error = None;

        match self.backup_mode {
            BackupMode::Direct => self.start_direct_backup(source_partition),
            BackupMode::ViaPE => self.start_pe_backup(source_partition),
        }
    }

    fn start_direct_backup(&mut self, source_partition: crate::core::disk::Partition) {
        let (progress_tx, progress_rx) = mpsc::channel::<DismProgress>();
        self.backup_progress_rx = Some(progress_rx);

        let capture_dir = format!("{}\\", source_partition.letter);
        let image_file = self.backup_save_path.clone();
        let name = self.backup_name.clone();
        let description = self.backup_description.clone();
        let is_incremental = self.backup_incremental;

        std::thread::spawn(move || {
            let dism = Dism::new();
            
            let result = if is_incremental && Path::new(&image_file).exists() {
                dism.append_image(&image_file, &capture_dir, &name, &description, Some(progress_tx.clone()))
            } else {
                dism.capture_image(&image_file, &capture_dir, &name, &description, Some(progress_tx.clone()))
            };

            match result {
                Ok(_) => {
                    let _ = progress_tx.send(DismProgress {
                        percentage: 100,
                        status: "备份完成".to_string(),
                    });
                }
                Err(e) => {
                    let _ = progress_tx.send(DismProgress {
                        percentage: 0,
                        status: format!("备份失败: {}", e),
                    });
                }
            }
        });
    }

    fn start_pe_backup(&mut self, source_partition: crate::core::disk::Partition) {
        println!("[BACKUP PE] ========== 开始PE备份准备 ==========");
        
        let (progress_tx, progress_rx) = mpsc::channel::<DismProgress>();
        self.backup_progress_rx = Some(progress_rx);

        let source_letter = source_partition.letter.clone();
        let save_path = self.backup_save_path.clone();
        let name = self.backup_name.clone();
        let description = self.backup_description.clone();
        let is_incremental = self.backup_incremental;
        let backup_format = self.backup_format.to_config_value();
        let swm_split_size = self.backup_swm_split_size;
        
        let pe_info = self.selected_pe_for_backup.and_then(|idx| {
            self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
        });

        std::thread::spawn(move || {
            // Step 1: 检查PE
            let _ = progress_tx.send(DismProgress {
                percentage: 10,
                status: "检查PE环境".to_string(),
            });
            
            let pe_info = match pe_info {
                Some(pe) => pe,
                None => {
                    let _ = progress_tx.send(DismProgress {
                        percentage: 0,
                        status: "备份失败: 未选择PE环境".to_string(),
                    });
                    return;
                }
            };
            
            let (pe_exists, pe_path) = crate::core::pe::PeManager::check_pe_exists(&pe_info.filename);
            if !pe_exists {
                let _ = progress_tx.send(DismProgress {
                    percentage: 0,
                    status: format!("备份失败: PE文件不存在 {}", pe_info.filename),
                });
                return;
            }

            // Step 2: 安装PE引导
            let _ = progress_tx.send(DismProgress {
                percentage: 30,
                status: "安装PE引导".to_string(),
            });
            
            let pe_manager = crate::core::pe::PeManager::new();
            if let Err(e) = pe_manager.boot_to_pe(&pe_path, &pe_info.display_name) {
                let _ = progress_tx.send(DismProgress {
                    percentage: 0,
                    status: format!("备份失败: PE引导安装失败 {}", e),
                });
                return;
            }

            // Step 3: 写入配置文件
            let _ = progress_tx.send(DismProgress {
                percentage: 60,
                status: "写入配置文件".to_string(),
            });
            
            // 找数据分区
            let data_partition = find_backup_data_partition(&source_letter);
            
            let backup_config = BackupConfig {
                save_path: save_path.clone(),
                name: name.clone(),
                description: description.clone(),
                source_partition: source_letter.clone(),
                incremental: is_incremental,
                format: backup_format,
                swm_split_size: swm_split_size,
            };
            
            if let Err(e) = ConfigFileManager::write_backup_config(&source_letter, &data_partition, &backup_config) {
                let _ = progress_tx.send(DismProgress {
                    percentage: 0,
                    status: format!("备份失败: 配置文件写入失败 {}", e),
                });
                return;
            }

            // Step 4: 完成
            let _ = progress_tx.send(DismProgress {
                percentage: 100,
                status: "PE备份准备完成".to_string(),
            });
            
            println!("[BACKUP PE] ========== PE备份准备结束 ==========");
        });
    }

    pub fn update_backup_progress(&mut self) {
        if !self.is_backing_up {
            return;
        }

        let mut should_finish = false;
        let mut error_msg: Option<String> = None;
        let mut latest_progress: Option<u8> = None;

        if let Some(ref rx) = self.backup_progress_rx {
            while let Ok(progress) = rx.try_recv() {
                latest_progress = Some(progress.percentage);
                
                if progress.percentage >= 100 {
                    should_finish = true;
                } else if progress.status.contains("失败") {
                    error_msg = Some(progress.status);
                    should_finish = true;
                }
            }
        }

        if let Some(p) = latest_progress {
            self.backup_progress = p;
        }

        if let Some(err) = error_msg {
            self.backup_error = Some(err);
        }

        if should_finish {
            self.is_backing_up = false;
            self.backup_progress_rx = None;
        }
    }

    /// 显示备份进度页面
    pub fn show_backup_progress(&mut self, ui: &mut egui::Ui) {
        ui.heading("备份进度");
        ui.separator();

        self.update_backup_progress();

        if !self.is_backing_up && self.backup_progress < 100 {
            ui.label("没有正在进行的备份任务");
            if ui.button("返回").clicked() {
                self.current_panel = Panel::SystemBackup;
            }
            return;
        }

        // 显示备份模式
        let mode_text = match self.backup_mode {
            BackupMode::Direct => "直接备份",
            BackupMode::ViaPE => "通过PE备份",
        };
        ui.label(format!("备份模式: {}", mode_text));

        ui.add_space(15.0);

        ui.label("备份进度:");
        ui.add(
            egui::ProgressBar::new(self.backup_progress as f32 / 100.0)
                .text(format!("{}%", self.backup_progress))
                .animate(true),
        );

        ui.add_space(20.0);

        if let Some(ref error) = self.backup_error {
            ui.colored_label(egui::Color32::RED, format!("错误: {}", error));
            ui.add_space(10.0);
        }

        if self.backup_progress >= 100 {
            match self.backup_mode {
                BackupMode::Direct => {
                    ui.colored_label(egui::Color32::GREEN, "备份完成！");
                    ui.add_space(10.0);
                    if ui.button("返回").clicked() {
                        self.current_panel = Panel::SystemBackup;
                    }
                }
                BackupMode::ViaPE => {
                    ui.colored_label(egui::Color32::GREEN, "PE环境准备完成！");
                    ui.label("系统将重启进入PE环境继续备份。");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("立即重启").clicked() {
                            let _ = crate::utils::cmd::create_command("shutdown")
                                .args(["/r", "/t", "5", "/c", "LetRecovery 即将重启到PE环境进行备份..."])
                                .spawn();
                        }
                        if ui.button("稍后重启").clicked() {
                            self.current_panel = Panel::SystemBackup;
                        }
                    });
                }
            }
        } else if self.is_backing_up {
            if ui.button("取消备份").clicked() {
                println!("[BACKUP] 用户取消备份");
                self.is_backing_up = false;
                self.current_panel = Panel::SystemBackup;
            }
        }
    }
}

/// 查找可用的备份数据分区
fn find_backup_data_partition(exclude_partition: &str) -> String {
    use crate::core::disk::DiskManager;
    
    let exclude_letter = exclude_partition.chars().next().unwrap_or('C').to_ascii_uppercase();
    
    // 遍历 A-Z 查找可用的固定磁盘分区
    for letter in b'A'..=b'Z' {
        let c = letter as char;
        
        // 跳过排除的分区
        if c == exclude_letter {
            continue;
        }
        
        // 跳过 X 盘（PE 系统盘）
        if c == 'X' {
            continue;
        }
        
        let partition_path = format!("{}:\\", c);
        if !Path::new(&partition_path).exists() {
            continue;
        }
        
        // 检查是否为光驱
        if DiskManager::is_cdrom(c) {
            continue;
        }
        
        // 检查是否为固定磁盘
        if !DiskManager::is_fixed_drive(c) {
            continue;
        }
        
        // 检查是否有足够空间（至少 100MB 用于配置文件）
        if let Some(free_space) = DiskManager::get_free_space_bytes(&format!("{}:", c)) {
            if free_space >= 100 * 1024 * 1024 {
                return format!("{}:", c);
            }
        }
    }
    
    // 如果没找到合适的，使用 C 盘
    "C:".to_string()
}
