use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::common::get_message_color;

impl App {
    // ==================== 分区对拷对话框 ====================

    /// 检查分区对拷异步操作结果
    pub(crate) fn check_partition_copy_async_operations(&mut self) {
        // 检查分区列表加载结果
        if let Some(ref rx) = self.partition_copy_partitions_rx {
            if let Ok(partitions) = rx.try_recv() {
                self.partition_copy_partitions = partitions;
                self.partition_copy_partitions_loading = false;
                self.partition_copy_partitions_rx = None;
                
                // 自动检查是否可以继续对拷
                self.update_partition_copy_resume_state();
            }
        }
        
        // 检查复制进度
        if let Some(ref rx) = self.partition_copy_progress_rx {
            // 使用 try_iter 获取所有可用的进度更新
            let mut latest_progress: Option<super::super::partition_copy::CopyProgress> = None;
            
            while let Ok(progress) = rx.try_recv() {
                latest_progress = Some(progress);
            }
            
            if let Some(progress) = latest_progress {
                // 更新日志
                if !progress.current_file.is_empty() && !progress.current_file.starts_with("正在") {
                    // 添加到日志（限制日志长度）
                    let log_line = if progress.completed {
                        tr!("[完成] {}\n", progress.current_file)
                    } else {
                        tr!("[复制] {}\n", progress.current_file)
                    };
                    self.partition_copy_log.push_str(&log_line);
                    
                    // 限制日志长度，保留最新的部分
                    const MAX_LOG_BYTES: usize = 100_000;
                    if self.partition_copy_log.len() > MAX_LOG_BYTES {
                        // 找到合适的截断点
                        let start = self.partition_copy_log.len() - MAX_LOG_BYTES / 2;
                        if let Some(newline_pos) = self.partition_copy_log[start..].find('\n') {
                            self.partition_copy_log = self.partition_copy_log[start + newline_pos + 1..].to_string();
                        }
                    }
                }
                
                // 更新消息
                if progress.completed {
                    let msg = if progress.failed_count > 0 {
                        tr!(
                            "复制完成！已复制 {} 个文件，跳过 {} 个，失败 {} 个",
                            progress.copied_count,
                            progress.skipped_count,
                            progress.failed_count
                        )
                    } else {
                        tr!(
                            "复制完成！已复制 {} 个文件，跳过 {} 个（已存在）",
                            progress.copied_count,
                            progress.skipped_count
                        )
                    };
                    self.partition_copy_message = msg;
                    self.partition_copy_copying = false;
                    self.partition_copy_progress_rx = None;
                    
                    // 刷新分区列表
                    self.start_load_copyable_partitions();
                } else if let Some(ref error) = progress.error {
                    self.partition_copy_message = tr!("错误: {}", error);
                    self.partition_copy_copying = false;
                    self.partition_copy_progress_rx = None;
                } else {
                    self.partition_copy_message = tr!(
                        "正在复制 {}/{}（跳过 {}）: {}",
                        progress.copied_count,
                        progress.total_count,
                        progress.skipped_count,
                        progress.current_file
                    );
                }
                
                self.partition_copy_progress = Some(progress);
            }
        }
    }

    /// 更新是否可以继续对拷的状态
    fn update_partition_copy_resume_state(&mut self) {
        if let (Some(source), Some(target)) = (&self.partition_copy_source, &self.partition_copy_target) {
            self.partition_copy_is_resume = super::super::partition_copy::can_resume_copy(source, target);
        } else {
            self.partition_copy_is_resume = false;
        }
    }

    /// 渲染分区对拷对话框
    pub fn render_partition_copy_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_partition_copy_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_copy = false;

        egui::Window::new(tr!("分区对拷"))
            .resizable(true)
            .default_width(650.0)
            .default_height(550.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("将源分区的所有文件复制到目标分区（支持断点续传）"));
                ui.add_space(10.0);

                if self.partition_copy_partitions_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测分区..."));
                    });
                } else if self.partition_copy_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        tr!("未找到可用的分区"),
                    );
                } else {
                    // 克隆分区列表避免借用冲突
                    let partitions_clone = self.partition_copy_partitions.clone();

                    // ========== 源分区选择 ==========
                    ui.horizontal(|ui| {
                        ui.label(tr!("请选择源分区:"));
                        let current_source = self.partition_copy_source.clone().unwrap_or_else(|| tr!("请选择"));
                        
                        egui::ComboBox::from_id_salt("partition_copy_source")
                            .selected_text(&current_source)
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for partition in &partitions_clone {
                                    let display = format!("{}", partition.letter);
                                    ui.selectable_value(
                                        &mut self.partition_copy_source,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });

                    ui.add_space(5.0);

                    // 源分区列表框
                    ui.group(|ui| {
                        ui.set_min_height(120.0);
                        ui.set_max_height(120.0);
                        
                        egui::ScrollArea::vertical()
                            .id_salt("source_partition_scroll")
                            .show(ui, |ui| {
                                // 表头
                                egui::Grid::new("source_partition_header")
                                    .num_columns(5)
                                    .spacing([10.0, 4.0])
                                    .min_col_width(80.0)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new(tr!("分区卷")).strong());
                                        ui.label(egui::RichText::new(tr!("总空间")).strong());
                                        ui.label(egui::RichText::new(tr!("已用空间")).strong());
                                        ui.label(egui::RichText::new(tr!("卷标")).strong());
                                        ui.label(egui::RichText::new(tr!("状态")).strong());
                                        ui.end_row();
                                    });

                                ui.separator();

                                // 分区列表
                                egui::Grid::new("source_partition_list")
                                    .num_columns(5)
                                    .spacing([10.0, 2.0])
                                    .min_col_width(80.0)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for partition in &partitions_clone {
                                            let is_selected = self.partition_copy_source.as_ref() == Some(&partition.letter);
                                            
                                            if ui.selectable_label(is_selected, &partition.letter).clicked() {
                                                self.partition_copy_source = Some(partition.letter.clone());
                                                self.update_partition_copy_resume_state();
                                            }
                                            
                                            ui.label(format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0));
                                            ui.label(format!("{:.1} GB", partition.used_size_mb as f64 / 1024.0));
                                            ui.label(if partition.label.is_empty() { "-" } else { &partition.label });
                                            ui.label(if partition.has_system { tr!("有系统") } else { tr!("无系统") });
                                            ui.end_row();
                                        }
                                    });
                            });
                    });

                    ui.add_space(15.0);

                    // ========== 目标分区选择 ==========
                    ui.horizontal(|ui| {
                        ui.label(tr!("请选择目标分区:"));
                        let current_target = self.partition_copy_target.clone().unwrap_or_else(|| tr!("请选择"));
                        
                        egui::ComboBox::from_id_salt("partition_copy_target")
                            .selected_text(&current_target)
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for partition in &partitions_clone {
                                    let display = format!("{}", partition.letter);
                                    ui.selectable_value(
                                        &mut self.partition_copy_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });

                    ui.add_space(5.0);

                    // 目标分区列表框
                    ui.group(|ui| {
                        ui.set_min_height(120.0);
                        ui.set_max_height(120.0);
                        
                        egui::ScrollArea::vertical()
                            .id_salt("target_partition_scroll")
                            .show(ui, |ui| {
                                // 表头
                                egui::Grid::new("target_partition_header")
                                    .num_columns(5)
                                    .spacing([10.0, 4.0])
                                    .min_col_width(80.0)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new(tr!("分区卷")).strong());
                                        ui.label(egui::RichText::new(tr!("总空间")).strong());
                                        ui.label(egui::RichText::new(tr!("已用空间")).strong());
                                        ui.label(egui::RichText::new(tr!("卷标")).strong());
                                        ui.label(egui::RichText::new(tr!("状态")).strong());
                                        ui.end_row();
                                    });

                                ui.separator();

                                // 分区列表
                                egui::Grid::new("target_partition_list")
                                    .num_columns(5)
                                    .spacing([10.0, 2.0])
                                    .min_col_width(80.0)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for partition in &partitions_clone {
                                            let is_selected = self.partition_copy_target.as_ref() == Some(&partition.letter);
                                            
                                            if ui.selectable_label(is_selected, &partition.letter).clicked() {
                                                self.partition_copy_target = Some(partition.letter.clone());
                                                self.update_partition_copy_resume_state();
                                            }
                                            
                                            ui.label(format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0));
                                            ui.label(format!("{:.1} GB", partition.used_size_mb as f64 / 1024.0));
                                            ui.label(if partition.label.is_empty() { "-" } else { &partition.label });
                                            ui.label(if partition.has_system { tr!("有系统") } else { tr!("无系统") });
                                            ui.end_row();
                                        }
                                    });
                            });
                    });
                }

                ui.add_space(15.0);

                // 显示复制日志（如果正在复制或已复制）
                if self.partition_copy_copying || !self.partition_copy_log.is_empty() {
                    ui.label(tr!("复制日志:"));
                    ui.group(|ui| {
                        ui.set_min_height(100.0);
                        ui.set_max_height(100.0);
                        
                        egui::ScrollArea::vertical()
                            .id_salt("partition_copy_log")
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.partition_copy_log.as_str())
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .interactive(false)
                                );
                            });
                    });
                    ui.add_space(10.0);
                }

                // 显示状态消息
                if !self.partition_copy_message.is_empty() {
                    let color = get_message_color(&self.partition_copy_message);
                    ui.colored_label(color, &self.partition_copy_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.partition_copy_copying {
                        ui.spinner();
                        ui.label(tr!("正在复制..."));
                    } else {
                        // 检查是否可以开始复制
                        let source_valid = self.partition_copy_source.is_some();
                        let target_valid = self.partition_copy_target.is_some();
                        let same_partition = source_valid && target_valid 
                            && self.partition_copy_source == self.partition_copy_target;
                        
                        let can_copy = source_valid && target_valid && !same_partition
                            && !self.partition_copy_partitions_loading;

                        // 根据是否可以继续显示不同的按钮文字
                        let button_text = if self.partition_copy_is_resume {
                            tr!("继续对拷")
                        } else {
                            tr!("开始对拷")
                        };

                        if ui
                            .add_enabled(can_copy, egui::Button::new(button_text))
                            .clicked()
                        {
                            if same_partition {
                                self.partition_copy_message = tr!("错误: 源分区和目标分区不能相同！");
                            } else {
                                do_copy = true;
                            }
                        }

                        // 如果选择了相同分区，显示错误提示
                        if same_partition {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 80, 80),
                                tr!("源分区和目标分区不能相同！")
                            );
                        }

                        if ui.button(tr!("刷新")).clicked() {
                            self.start_load_copyable_partitions();
                        }

                        if ui.button(tr!("关闭")).clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_copy {
            self.start_partition_copy();
        }

        if should_close {
            self.show_partition_copy_dialog = false;
        }
    }

    /// 启动后台加载可复制分区列表
    pub fn start_load_copyable_partitions(&mut self) {
        if self.partition_copy_partitions_loading {
            return;
        }

        self.partition_copy_partitions_loading = true;
        self.partition_copy_partitions.clear();

        let (tx, rx) = mpsc::channel();
        self.partition_copy_partitions_rx = Some(rx);

        std::thread::spawn(move || {
            let partitions = super::super::partition_copy::get_copyable_partitions();
            let _ = tx.send(partitions);
        });
    }

    /// 启动分区对拷操作
    fn start_partition_copy(&mut self) {
        let source = match &self.partition_copy_source {
            Some(s) => s.clone(),
            None => {
                self.partition_copy_message = tr!("请选择源分区");
                return;
            }
        };

        let target = match &self.partition_copy_target {
            Some(t) => t.clone(),
            None => {
                self.partition_copy_message = tr!("请选择目标分区");
                return;
            }
        };

        if source == target {
            self.partition_copy_message = tr!("错误: 源分区和目标分区不能相同！");
            return;
        }

        // 检查目标空间
        if let Err(e) = super::super::partition_copy::check_target_space(&source, &target) {
            self.partition_copy_message = e;
            return;
        }

        self.partition_copy_copying = true;
        self.partition_copy_log.clear();
        self.partition_copy_message = tr!("正在准备复制...");

        let is_resume = self.partition_copy_is_resume;
        
        let (tx, rx) = mpsc::channel();
        self.partition_copy_progress_rx = Some(rx);

        std::thread::spawn(move || {
            super::super::partition_copy::execute_partition_copy(&source, &target, tx, is_resume);
        });
    }
}
