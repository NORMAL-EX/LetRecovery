//! GHO密码查看对话框模块
//!
//! 提供查看GHO镜像文件密码的UI界面

use egui;
use std::sync::mpsc;

use crate::app::App;
use crate::core::gho_password::read_gho_password;
use crate::tr;
use super::types::GhoPasswordResult;

impl App {
    /// 渲染GHO密码查看对话框
    pub fn render_gho_password_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_gho_password_dialog {
            return;
        }

        let mut should_close = false;

        egui::Window::new(tr!("查看GHO密码"))
            .resizable(true)
            .default_width(500.0)
            .default_height(300.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("查看Ghost镜像文件(.gho)的密码信息"));
                ui.add_space(10.0);

                // 文件路径输入
                ui.horizontal(|ui| {
                    ui.label(tr!("GHO文件路径:"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gho_password_file_path)
                            .hint_text(tr!("输入或选择GHO文件路径"))
                            .desired_width(300.0),
                    );

                    if ui.button(tr!("浏览...")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter(tr!("GHO镜像文件"), &["gho", "GHO", "ghs", "GHS"])
                            .add_filter(tr!("所有文件"), &["*"])
                            .pick_file()
                        {
                            self.gho_password_file_path = path.to_string_lossy().to_string();
                        }
                    }
                });

                ui.add_space(15.0);

                // 查看按钮
                ui.horizontal(|ui| {
                    let can_view = !self.gho_password_file_path.is_empty() && !self.gho_password_loading;
                    
                    if ui.add_enabled(can_view, egui::Button::new(tr!("查看密码"))).clicked() {
                        self.start_read_gho_password();
                    }

                    if self.gho_password_loading {
                        ui.spinner();
                        ui.label(tr!("正在读取..."));
                    }
                });

                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);

                // 显示结果
                if let Some(ref result) = self.gho_password_result {
                    // 显示文件路径
                    ui.horizontal(|ui| {
                        ui.label(tr!("文件:"));
                        ui.label(&result.file_path);
                    });
                    
                    ui.add_space(5.0);

                    // 显示有效性状态
                    if result.is_valid {
                        ui.colored_label(egui::Color32::from_rgb(0, 180, 0), tr!("有效的GHO文件"));
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(255, 80, 80), tr!("无效的GHO文件"));
                    }
                    
                    ui.add_space(5.0);

                    // 显示密码信息
                    if result.is_valid {
                        if result.has_password {
                            ui.colored_label(egui::Color32::from_rgb(255, 165, 0), tr!("已设置密码保护"));

                            ui.horizontal(|ui| {
                                ui.label(tr!("密码长度:"));
                                ui.label(tr!("{} 字符", result.password_length));
                            });

                            if let Some(ref pwd) = result.password {
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label(tr!("密码:"));
                                    // 使用可选择的文本框显示密码，方便复制
                                    let mut pwd_display = pwd.clone();
                                    ui.add(
                                        egui::TextEdit::singleline(&mut pwd_display)
                                            .desired_width(200.0)
                                            .interactive(true)
                                    );

                                    if ui.button(tr!("复制")).clicked() {
                                        ui.ctx().copy_text(pwd.clone());
                                    }
                                });
                            } else if !result.message.is_empty() {
                                ui.add_space(5.0);
                                ui.colored_label(egui::Color32::YELLOW, format!("{}", result.message));
                            }
                        } else {
                            ui.colored_label(egui::Color32::from_rgb(0, 180, 0), tr!("未设置密码保护"));
                        }
                    }
                    
                    // 显示错误消息
                    if !result.is_valid && !result.message.is_empty() {
                        ui.add_space(5.0);
                        ui.colored_label(egui::Color32::from_rgb(255, 80, 80), &result.message);
                    }
                }

                ui.add_space(20.0);

                // 关闭按钮
                ui.horizontal(|ui| {
                    if ui.button(tr!("关闭")).clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_gho_password_dialog = false;
        }
    }

    /// 启动后台读取GHO密码
    fn start_read_gho_password(&mut self) {
        if self.gho_password_loading {
            return;
        }

        let file_path = self.gho_password_file_path.clone();
        if file_path.is_empty() {
            return;
        }

        self.gho_password_loading = true;
        self.gho_password_result = None;

        let (tx, rx) = mpsc::channel();
        self.gho_password_rx = Some(rx);

        std::thread::spawn(move || {
            let info = read_gho_password(&file_path);
            let result = GhoPasswordResult {
                file_path,
                is_valid: info.is_valid_gho,
                has_password: info.has_password,
                password: info.password,
                password_length: info.password_length,
                message: info.error.unwrap_or_default(),
            };
            let _ = tx.send(result);
        });
    }

    /// 检查GHO密码读取结果
    pub fn check_gho_password_result(&mut self) {
        if let Some(ref rx) = self.gho_password_rx {
            if let Ok(result) = rx.try_recv() {
                self.gho_password_result = Some(result);
                self.gho_password_loading = false;
                self.gho_password_rx = None;
            }
        }
    }
}
