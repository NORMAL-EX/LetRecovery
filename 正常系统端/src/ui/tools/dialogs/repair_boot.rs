use egui;
use crate::tr;
use crate::app::App;
use super::common::{format_partition_display, get_message_color};

impl App {
    // ==================== 一键修复引导对话框 ====================

    /// 渲染一键修复引导对话框
    pub fn render_repair_boot_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_repair_boot_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_repair = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;

        egui::Window::new(tr!("一键修复引导"))
            .resizable(false)
            .default_width(450.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("修复Windows系统的启动引导"));
                ui.add_space(10.0);

                // 分区选择
                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测Windows分区..."));
                    });
                } else if windows_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 100, 100),
                        tr!("未检测到包含Windows系统的分区"),
                    );
                    ui.add_space(5.0);
                    ui.label(tr!("请确保目标分区包含有效的Windows系统"));
                } else {
                    ui.horizontal(|ui| {
                        ui.label(tr!("选择目标系统分区:"));

                        let current_text = self
                            .repair_boot_selected_partition
                            .as_ref()
                            .map(|letter| format_partition_display(&windows_partitions, letter))
                            .unwrap_or_else(|| tr!("请选择"));

                        egui::ComboBox::from_id_salt("repair_boot_partition_select")
                            .selected_text(current_text)
                            .width(250.0)
                            .show_ui(ui, |ui| {
                                for partition in &windows_partitions {
                                    let display = format!(
                                        "{} [{}] [{}]",
                                        partition.letter,
                                        partition.windows_version,
                                        partition.architecture
                                    );
                                    ui.selectable_value(
                                        &mut self.repair_boot_selected_partition,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });

                    // 显示所选分区的详细信息
                    if let Some(ref selected) = self.repair_boot_selected_partition {
                        if let Some(partition) = windows_partitions.iter().find(|p| &p.letter == selected) {
                            ui.add_space(10.0);
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(tr!("Windows版本:"));
                                    ui.label(&partition.windows_version);
                                });
                                ui.horizontal(|ui| {
                                    ui.label(tr!("系统架构:"));
                                    ui.label(&partition.architecture);
                                });
                            });
                        }
                    }
                }

                ui.add_space(15.0);

                // 消息显示
                if !self.repair_boot_message.is_empty() {
                    let color = get_message_color(&self.repair_boot_message);
                    ui.colored_label(color, &self.repair_boot_message);
                    ui.add_space(10.0);
                }

                // 进度指示
                if self.repair_boot_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在修复引导..."));
                    });
                    ui.add_space(10.0);
                }

                ui.separator();
                ui.add_space(5.0);

                // 按钮
                ui.horizontal(|ui| {
                    let can_repair = !self.repair_boot_loading 
                        && self.repair_boot_selected_partition.is_some()
                        && !windows_partitions.is_empty();

                    if ui
                        .add_enabled(can_repair, egui::Button::new(tr!("开始修复")))
                        .clicked()
                    {
                        do_repair = true;
                    }

                    if ui
                        .add_enabled(!self.repair_boot_loading, egui::Button::new(tr!("刷新")))
                        .clicked()
                    {
                        self.refresh_windows_partitions_cache();
                    }

                    if ui.button(tr!("关闭")).clicked() {
                        should_close = true;
                    }
                });
            });

        // 执行修复
        if do_repair {
            self.repair_boot_action();
        }

        // 关闭对话框
        if should_close {
            self.show_repair_boot_dialog = false;
            self.repair_boot_message.clear();
            self.repair_boot_selected_partition = None;
        }
    }
}
