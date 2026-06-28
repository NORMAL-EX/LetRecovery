use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::super::types::DriverBackupMode;
use super::common::{format_partition_display, get_message_color};

impl App {
    /// 渲染驱动备份还原对话框
    pub fn render_driver_backup_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_driver_backup_dialog {
            return;
        }

        let mut should_close = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;

        egui::Window::new(tr!("驱动备份还原"))
            .resizable(false)
            .default_width(500.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("导出或导入系统驱动"));
                ui.add_space(10.0);

                // 模式选择
                ui.horizontal(|ui| {
                    ui.label(tr!("操作模式:"));
                    ui.radio_value(&mut self.driver_backup_mode, DriverBackupMode::Export, tr!("导出驱动"));
                    ui.radio_value(&mut self.driver_backup_mode, DriverBackupMode::Import, tr!("导入驱动"));
                });

                ui.add_space(10.0);

                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测Windows分区..."));
                    });
                } else {
                    // 根据模式显示不同选项
                    match self.driver_backup_mode {
                        DriverBackupMode::Export => {
                            ui.horizontal(|ui| {
                                ui.label(tr!("源系统分区:"));

                                let current_text = self
                                    .driver_backup_target
                                    .as_ref()
                                    .map(|letter| format_partition_display(&windows_partitions, letter))
                                    .unwrap_or_else(|| tr!("请选择"));

                                egui::ComboBox::from_id_salt("driver_backup_source")
                                    .selected_text(current_text)
                                    .show_ui(ui, |ui| {
                                        for partition in &windows_partitions {
                                            let display = format!(
                                                "{} [{}] [{}]",
                                                partition.letter,
                                                partition.windows_version,
                                                partition.architecture
                                            );
                                            ui.selectable_value(
                                                &mut self.driver_backup_target,
                                                Some(partition.letter.clone()),
                                                display,
                                            );
                                        }
                                    });
                            });

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label(tr!("保存目录:"));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.driver_backup_path)
                                        .desired_width(300.0),
                                );
                                if ui.button(tr!("浏览...")).clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        self.driver_backup_path = path.to_string_lossy().to_string();
                                    }
                                }
                            });
                        }
                        DriverBackupMode::Import => {
                            ui.horizontal(|ui| {
                                ui.label(tr!("目标系统分区:"));

                                let current_text = self
                                    .driver_backup_target
                                    .as_ref()
                                    .map(|letter| format_partition_display(&windows_partitions, letter))
                                    .unwrap_or_else(|| tr!("请选择"));

                                egui::ComboBox::from_id_salt("driver_import_target")
                                    .selected_text(current_text)
                                    .show_ui(ui, |ui| {
                                        for partition in &windows_partitions {
                                            let display = format!(
                                                "{} [{}] [{}]",
                                                partition.letter,
                                                partition.windows_version,
                                                partition.architecture
                                            );
                                            ui.selectable_value(
                                                &mut self.driver_backup_target,
                                                Some(partition.letter.clone()),
                                                display,
                                            );
                                        }
                                    });
                            });

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label(tr!("驱动目录:"));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.driver_backup_path)
                                        .desired_width(300.0),
                                );
                                if ui.button(tr!("浏览...")).clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        self.driver_backup_path = path.to_string_lossy().to_string();
                                    }
                                }
                            });
                        }
                    }
                }

                ui.add_space(15.0);

                // 状态消息
                if !self.driver_backup_message.is_empty() {
                    let color = get_message_color(&self.driver_backup_message);
                    ui.colored_label(color, &self.driver_backup_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.driver_backup_loading {
                        ui.spinner();
                        ui.label(tr!("正在处理，请稍候..."));
                    } else {
                        let button_label = match self.driver_backup_mode {
                            DriverBackupMode::Export => tr!("导出"),
                            DriverBackupMode::Import => tr!("导入"),
                        };

                        let can_execute = !self.driver_backup_path.is_empty()
                            && self.driver_backup_target.is_some()
                            && !is_loading_partitions;

                        if ui
                            .add_enabled(can_execute, egui::Button::new(button_label))
                            .clicked()
                        {
                            self.start_driver_backup_action();
                        }
                    }

                    if ui.button(tr!("关闭")).clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_driver_backup_dialog = false;
        }
    }

    /// 启动后台驱动备份/还原操作
    fn start_driver_backup_action(&mut self) {
        if self.driver_backup_path.is_empty() {
            self.driver_backup_message = tr!("请指定目录路径");
            return;
        }

        let target = match &self.driver_backup_target {
            Some(t) => t.clone(),
            None => {
                self.driver_backup_message = tr!("请选择系统分区");
                return;
            }
        };

        let path = self.driver_backup_path.clone();
        let mode = self.driver_backup_mode;

        self.driver_backup_loading = true;
        self.driver_backup_message = match mode {
            DriverBackupMode::Export => tr!("正在导出驱动，请稍候..."),
            DriverBackupMode::Import => tr!("正在导入驱动，请稍候..."),
        };

        let (tx, rx) = mpsc::channel();
        self.driver_operation_rx = Some(rx);

        std::thread::spawn(move || {
            let dism = crate::core::dism::Dism::new();
            
            let result = match mode {
                DriverBackupMode::Export => {
                    match dism.export_drivers_from_system(&target, &path) {
                        Ok(_) => Ok(tr!("驱动导出成功: {} -> {}", target, path)),
                        Err(e) => Err(tr!("驱动导出失败: {}", e)),
                    }
                }
                DriverBackupMode::Import => {
                    // 检查驱动目录是否存在
                    if !std::path::Path::new(&path).exists() {
                        Err(tr!("驱动目录不存在: {}", path))
                    } else {
                        match dism.add_drivers_offline(&target, &path) {
                            Ok(_) => Ok(tr!("驱动导入成功！")),
                            Err(e) => Err(tr!("驱动导入失败: {}", e)),
                        }
                    }
                }
            };
            
            let _ = tx.send(result);
        });
    }
}
