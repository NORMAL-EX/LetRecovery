use egui;
use crate::tr;
use crate::app::App;
use super::super::software::{truncate_string, save_software_list_to_file, get_installed_software};

impl App {
    /// 渲染软件列表对话框
    pub fn render_software_list_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_software_list_dialog {
            return;
        }

        let mut should_close = false;
        let mut save_path: Option<std::path::PathBuf> = None;
        
        // 克隆数据避免借用冲突
        let software_list_clone = self.software_list.clone();
        let is_loading = self.software_list_loading;

        egui::Window::new(tr!("已安装软件列表"))
            .resizable(true)
            .default_width(500.0)
            .default_height(450.0)
            .show(ui.ctx(), |ui| {
                if is_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在加载软件列表..."));
                    });
                } else {
                    ui.label(tr!("共 {} 个软件", software_list_clone.len()));
                    ui.add_space(5.0);

                    // 表头
                    egui::Grid::new("software_header")
                        .num_columns(3)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(tr!("软件名称")).strong());
                            ui.label(egui::RichText::new(tr!("版本")).strong());
                            ui.label(egui::RichText::new(tr!("发布者")).strong());
                            ui.end_row();
                        });

                    ui.separator();

                    // 软件列表
                    egui::ScrollArea::vertical()
                        .max_height(350.0)
                        .show(ui, |ui| {
                            egui::Grid::new("software_list")
                                .num_columns(3)
                                .spacing([8.0, 2.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    for software in &software_list_clone {
                                        ui.label(truncate_string(&software.name, 30));
                                        ui.label(truncate_string(&software.version, 15));
                                        ui.label(truncate_string(&software.publisher, 20));
                                        ui.end_row();
                                    }
                                });
                        });
                }

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button(tr!("保存列表为TXT")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("installed_software.txt")
                            .add_filter(tr!("文本文件"), &["txt"])
                            .save_file()
                        {
                            save_path = Some(path);
                        }
                    }

                    if ui.button(tr!("关闭")).clicked() {
                        should_close = true;
                    }
                });
            });

        // 在窗口渲染之后处理保存
        if let Some(path) = save_path {
            save_software_list_to_file(&path, &software_list_clone);
        }

        if should_close {
            self.show_software_list_dialog = false;
        }
    }

    /// 初始化软件列表对话框
    pub fn init_software_list_dialog(&mut self) {
        self.show_software_list_dialog = true;
        self.software_list_loading = true;
        self.software_list = get_installed_software();
        self.software_list_loading = false;
    }
}
