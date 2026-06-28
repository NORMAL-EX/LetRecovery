use egui;
use std::collections::HashSet;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::common::get_message_color;

impl App {
    // ==================== 批量格式化对话框 ====================

    /// 渲染批量格式化对话框
    pub fn render_batch_format_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_batch_format_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_format = false;

        egui::Window::new(tr!("批量格式化"))
            .resizable(true)
            .default_width(500.0)
            .default_height(400.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("选择要格式化的分区（系统盘已自动隐藏）"));
                ui.add_space(10.0);

                if self.batch_format_partitions_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测分区..."));
                    });
                } else if self.batch_format_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        tr!("未找到可格式化的分区"),
                    );
                } else {
                    // 全选/反选按钮
                    ui.horizontal(|ui| {
                        if ui.button(tr!("全选")).clicked() {
                            for p in &self.batch_format_partitions {
                                self.batch_format_selected.insert(p.letter.clone());
                            }
                        }
                        if ui.button(tr!("反选")).clicked() {
                            let current: HashSet<_> = self.batch_format_selected.clone();
                            self.batch_format_selected.clear();
                            for p in &self.batch_format_partitions {
                                if !current.contains(&p.letter) {
                                    self.batch_format_selected.insert(p.letter.clone());
                                }
                            }
                        }
                        ui.label(tr!("已选择 {} 个分区", self.batch_format_selected.len()));
                    });

                    ui.add_space(5.0);
                    ui.separator();

                    // 分区列表
                    egui::ScrollArea::vertical()
                        .max_height(250.0)
                        .show(ui, |ui| {
                            for partition in &self.batch_format_partitions.clone() {
                                let mut selected = self.batch_format_selected.contains(&partition.letter);
                                
                                let display_text = tr!(
                                    "{} [{}] - {} ({} GB / {} GB 可用)",
                                    partition.letter,
                                    if partition.label.is_empty() { "无标签" } else { &partition.label },
                                    partition.file_system,
                                    format!("{:.1}", partition.total_size_mb as f64 / 1024.0),
                                    format!("{:.1}", partition.free_size_mb as f64 / 1024.0),
                                );

                                if ui.checkbox(&mut selected, display_text).changed() {
                                    if selected {
                                        self.batch_format_selected.insert(partition.letter.clone());
                                    } else {
                                        self.batch_format_selected.remove(&partition.letter);
                                    }
                                }
                            }
                        });
                }

                ui.add_space(10.0);

                // 显示状态消息
                if !self.batch_format_message.is_empty() {
                    let color = get_message_color(&self.batch_format_message);
                    ui.colored_label(color, &self.batch_format_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.batch_format_loading {
                        ui.spinner();
                        ui.label(tr!("正在格式化..."));
                    } else {
                        let can_format = !self.batch_format_selected.is_empty()
                            && !self.batch_format_partitions_loading;

                        if ui
                            .add_enabled(can_format, egui::Button::new(tr!("应用（格式化选中分区）")))
                            .clicked()
                        {
                            // 显示确认对话框
                            do_format = true;
                        }

                        if ui.button(tr!("刷新")).clicked() {
                            self.start_load_formatable_partitions();
                        }

                        if ui.button(tr!("关闭")).clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_format && !self.batch_format_selected.is_empty() {
            // 开始格式化
            self.start_batch_format();
        }

        if should_close {
            self.show_batch_format_dialog = false;
        }
    }

    /// 启动后台加载可格式化分区
    pub fn start_load_formatable_partitions(&mut self) {
        if self.batch_format_partitions_loading {
            return;
        }

        self.batch_format_partitions_loading = true;
        self.batch_format_partitions.clear();

        let (tx, rx) = mpsc::channel();
        self.batch_format_partitions_rx = Some(rx);

        std::thread::spawn(move || {
            let partitions = super::super::batch_format::get_formatable_partitions();
            let _ = tx.send(partitions);
        });
    }

    /// 启动后台批量格式化
    fn start_batch_format(&mut self) {
        if self.batch_format_loading {
            return;
        }

        self.batch_format_loading = true;
        self.batch_format_message = tr!("正在格式化分区...");

        let selected: Vec<String> = self.batch_format_selected.iter().cloned().collect();
        let (tx, rx) = mpsc::channel();
        self.batch_format_rx = Some(rx);

        std::thread::spawn(move || {
            let result = super::super::batch_format::batch_format_partitions(&selected, "新加卷", "NTFS");
            let _ = tx.send(result);
        });
    }
}
