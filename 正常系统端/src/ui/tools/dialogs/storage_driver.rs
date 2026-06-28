use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::common::{format_partition_display, get_message_color};

impl App {
    /// 渲染导入存储驱动对话框
    pub fn render_import_storage_driver_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_import_storage_driver_dialog {
            return;
        }

        let mut should_close = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;

        egui::Window::new(tr!("导入硬盘控制器驱动"))
            .resizable(false)
            .default_width(450.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("将 Intel VMD / Apple SSD / Visior 等硬盘控制器驱动导入到离线系统"));
                ui.add_space(10.0);

                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测Windows分区..."));
                    });
                } else if windows_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        tr!("未找到包含 Windows 系统的分区"),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label(tr!("目标分区:"));

                        let current_text = self
                            .import_storage_driver_target
                            .as_ref()
                            .map(|letter| {
                                format_partition_display(&windows_partitions, letter)
                            })
                            .unwrap_or_else(|| tr!("请选择"));

                        egui::ComboBox::from_id_salt("import_storage_driver_partition")
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
                                        &mut self.import_storage_driver_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });
                }

                ui.add_space(15.0);

                // 状态消息
                if !self.import_storage_driver_message.is_empty() {
                    let color = get_message_color(&self.import_storage_driver_message);
                    ui.colored_label(color, &self.import_storage_driver_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    let can_import = self.import_storage_driver_target.is_some()
                        && !self.import_storage_driver_loading
                        && !is_loading_partitions;

                    if self.import_storage_driver_loading {
                        ui.spinner();
                        ui.label(tr!("正在导入驱动..."));
                    } else {
                        if ui.add_enabled(can_import, egui::Button::new(tr!("导入驱动"))).clicked() {
                            self.start_import_storage_driver();
                        }
                    }

                    if ui.button(tr!("关闭")).clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_import_storage_driver_dialog = false;
        }
    }

    /// 启动后台导入存储驱动
    fn start_import_storage_driver(&mut self) {
        let target = match &self.import_storage_driver_target {
            Some(t) => t.clone(),
            None => {
                self.import_storage_driver_message = tr!("请先选择目标分区");
                return;
            }
        };

        // 检查驱动目录是否存在
        let driver_dir = crate::utils::path::get_drivers_dir()
            .join("storage_controller");

        if !driver_dir.exists() {
            self.import_storage_driver_message =
                tr!("驱动目录不存在: {}", driver_dir.display());
            return;
        }

        self.import_storage_driver_loading = true;
        self.import_storage_driver_message = tr!("正在导入驱动...");

        let driver_dir_str = driver_dir.to_string_lossy().to_string();
        let (tx, rx) = mpsc::channel();
        self.storage_driver_rx = Some(rx);

        std::thread::spawn(move || {
            let dism = crate::core::dism::Dism::new();
            let result = match dism.add_drivers_offline(&target, &driver_dir_str) {
                Ok(_) => Ok(tr!("驱动导入成功！")),
                Err(e) => Err(tr!("驱动导入失败: {}", e)),
            };
            let _ = tx.send(result);
        });
    }
}
