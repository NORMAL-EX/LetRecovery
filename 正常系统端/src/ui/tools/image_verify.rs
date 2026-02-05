//! 镜像校验对话框模块
//!
//! 提供镜像完整性校验的UI界面，支持：
//! - WIM/ESD/SWM 镜像校验
//! - GHO 镜像校验
//! - ISO 镜像校验（自动挂载并检查内部镜像）

use egui;
use std::sync::mpsc;
use std::sync::atomic::Ordering;

use crate::app::App;
use crate::core::image_verify::{ImageType, ImageVerifier, VerifyProgress, VerifyStatus};
use super::types::ImageVerifyResult;

impl App {
    /// 渲染镜像校验对话框
    pub fn render_image_verify_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_image_verify_dialog {
            return;
        }

        let mut should_close = false;

        egui::Window::new("镜像校验")
            .resizable(true)
            .default_width(600.0)
            .default_height(450.0)
            .show(ui.ctx(), |ui| {
                ui.label("校验镜像文件的完整性，支持 WIM、ESD、SWM、GHO、ISO 格式");
                ui.add_space(10.0);

                // 文件路径输入区域
                ui.horizontal(|ui| {
                    ui.label("镜像文件:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.image_verify_file_path)
                            .hint_text("输入或选择镜像文件路径")
                            .desired_width(380.0),
                    );

                    let can_browse = !self.image_verify_loading;
                    if ui.add_enabled(can_browse, egui::Button::new("浏览...")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("系统镜像", &["wim", "esd", "swm", "gho", "ghs", "iso"])
                            .add_filter("WIM/ESD/SWM", &["wim", "esd", "swm"])
                            .add_filter("GHO", &["gho", "ghs"])
                            .add_filter("ISO", &["iso"])
                            .add_filter("所有文件", &["*"])
                            .pick_file()
                        {
                            self.image_verify_file_path = path.to_string_lossy().to_string();
                            // 清除之前的结果
                            self.image_verify_result = None;
                        }
                    }
                });

                ui.add_space(15.0);

                // 校验按钮和进度
                ui.horizontal(|ui| {
                    let can_verify = !self.image_verify_file_path.is_empty() && !self.image_verify_loading;

                    if ui.add_enabled(can_verify, egui::Button::new("开始校验")).clicked() {
                        self.start_image_verify();
                    }

                    if self.image_verify_loading {
                        // 显示取消按钮
                        if ui.button("❌ 取消").clicked() {
                            self.cancel_image_verify();
                        }
                        
                        ui.add_space(10.0);
                        ui.spinner();
                        
                        // 显示进度信息
                        if let Some(ref progress) = self.image_verify_progress {
                            ui.label(format!("{}% - {}", progress.percentage, progress.status));
                        } else {
                            ui.label("正在初始化...");
                        }
                    }
                });

                // 进度条
                if self.image_verify_loading {
                    ui.add_space(10.0);
                    let progress = self.image_verify_progress
                        .as_ref()
                        .map(|p| p.percentage as f32 / 100.0)
                        .unwrap_or(0.0);
                    ui.add(egui::ProgressBar::new(progress).show_percentage());
                }

                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);

                // 显示校验结果
                if let Some(ref result) = self.image_verify_result {
                    Self::render_verify_result(ui, result);
                } else if !self.image_verify_loading {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        "请选择镜像文件并点击「开始校验」",
                    );
                }

                ui.add_space(20.0);

                // 关闭按钮
                ui.horizontal(|ui| {
                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_image_verify_dialog = false;
            // 如果正在校验，取消它
            if self.image_verify_loading {
                self.cancel_image_verify();
            }
        }
    }

    /// 渲染校验结果
    fn render_verify_result(ui: &mut egui::Ui, result: &ImageVerifyResult) {
        // 文件信息
        ui.horizontal(|ui| {
            ui.label("文件:");
            ui.label(&result.file_path);
        });

        ui.horizontal(|ui| {
            ui.label("类型:");
            ui.label(&result.image_type);
        });

        ui.horizontal(|ui| {
            ui.label("大小:");
            ui.label(Self::format_file_size(result.file_size));
        });

        ui.add_space(10.0);

        // 校验状态（使用醒目的颜色）
        if result.is_valid {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(0, 200, 0),
                    "✅ 校验通过",
                );
            });
        } else {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 80, 80),
                    format!("❌ {}", result.status_text),
                );
            });
        }

        ui.add_space(5.0);

        // 详细消息
        if !result.message.is_empty() {
            ui.horizontal(|ui| {
                ui.label("说明:");
                ui.label(&result.message);
            });
        }

        // 镜像数量信息
        if result.image_count > 0 {
            ui.horizontal(|ui| {
                ui.label("镜像数量:");
                ui.label(format!("{}", result.image_count));
            });
        }

        if result.part_count > 1 {
            ui.horizontal(|ui| {
                ui.label("分卷数量:");
                ui.label(format!("{}", result.part_count));
            });
        }

        // 详细信息列表
        if !result.details.is_empty() {
            ui.add_space(10.0);
            ui.label("详细信息:");
            
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .show(ui, |ui| {
                    for detail in &result.details {
                        ui.horizontal(|ui| {
                            ui.label("•");
                            ui.label(detail);
                        });
                    }
                });
        }
    }

    /// 格式化文件大小
    fn format_file_size(size: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if size >= GB {
            format!("{:.2} GB", size as f64 / GB as f64)
        } else if size >= MB {
            format!("{:.2} MB", size as f64 / MB as f64)
        } else if size >= KB {
            format!("{:.2} KB", size as f64 / KB as f64)
        } else {
            format!("{} 字节", size)
        }
    }

    /// 开始镜像校验
    fn start_image_verify(&mut self) {
        if self.image_verify_loading {
            return;
        }

        let file_path = self.image_verify_file_path.clone();
        if file_path.is_empty() {
            return;
        }

        // 检查文件是否存在
        if !std::path::Path::new(&file_path).exists() {
            self.image_verify_result = Some(ImageVerifyResult {
                file_path: file_path.clone(),
                image_type: ImageType::from_extension(&file_path).to_string(),
                is_valid: false,
                status_text: "文件不存在".to_string(),
                message: "请检查文件路径是否正确".to_string(),
                ..Default::default()
            });
            return;
        }

        self.image_verify_loading = true;
        self.image_verify_result = None;
        self.image_verify_progress = Some(VerifyProgress {
            percentage: 0,
            status: "正在初始化...".to_string(),
            current_item: String::new(),
        });

        // 创建进度通道
        let (progress_tx, progress_rx) = mpsc::channel();
        self.image_verify_progress_rx = Some(progress_rx);

        // 创建结果通道
        let (result_tx, result_rx) = mpsc::channel();
        self.image_verify_result_rx = Some(result_rx);

        // 创建校验器并保存取消标志
        let verifier = ImageVerifier::new();
        self.image_verify_cancel_flag = Some(verifier.get_cancel_flag());

        // 在后台线程中执行校验
        std::thread::spawn(move || {
            println!("[IMAGE VERIFY] 开始校验: {}", file_path);

            let result = verifier.verify(&file_path, Some(progress_tx));

            println!("[IMAGE VERIFY] 校验完成: {:?}", result.status);

            // 转换为 UI 使用的结果类型
            let ui_result = ImageVerifyResult {
                file_path: result.file_path,
                image_type: result.image_type.to_string(),
                is_valid: result.status == VerifyStatus::Valid,
                status_text: result.status.to_string(),
                file_size: result.file_size,
                image_count: result.image_count,
                part_count: result.part_count,
                message: result.message,
                details: result.details,
            };

            let _ = result_tx.send(ui_result);
        });
    }

    /// 取消镜像校验
    fn cancel_image_verify(&mut self) {
        if let Some(ref cancel_flag) = self.image_verify_cancel_flag {
            cancel_flag.store(true, Ordering::SeqCst);
            println!("[IMAGE VERIFY] 已发送取消请求");
        }
    }

    /// 检查镜像校验状态（在主循环中调用）
    pub fn check_image_verify_status(&mut self) {
        // 检查进度更新
        if let Some(ref rx) = self.image_verify_progress_rx {
            while let Ok(progress) = rx.try_recv() {
                self.image_verify_progress = Some(progress);
            }
        }

        // 检查结果
        if let Some(ref rx) = self.image_verify_result_rx {
            if let Ok(result) = rx.try_recv() {
                self.image_verify_result = Some(result);
                self.image_verify_loading = false;
                self.image_verify_progress = None;
                self.image_verify_progress_rx = None;
                self.image_verify_result_rx = None;
                self.image_verify_cancel_flag = None;
            }
        }
    }
}
