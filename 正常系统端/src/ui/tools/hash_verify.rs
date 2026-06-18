//! 文件哈希（SHA-256）校验对话框
//!
//! 为任意文件（WIM/ESD/ISO/GHO 等镜像，或普通文件）计算 SHA-256，
//! 并可与用户提供的「期望哈希」比对，用于核对下载完整性。
//! 与「镜像校验」的 wimlib 内部完整性校验互补（后者只覆盖 WIM 系列）。

use egui;
use std::sync::mpsc;

use crate::app::App;

/// 哈希校验结果
#[derive(Clone, Default)]
pub struct HashVerifyResult {
    pub file_path: String,
    pub file_size: u64,
    /// 计算出的 SHA-256（小写十六进制）
    pub sha256: String,
    /// 用户提供的期望值（原样）
    pub expected: String,
    /// 是否与期望值匹配；None 表示未提供期望值
    pub matched: Option<bool>,
    /// 出错信息（读取/计算失败）
    pub error: Option<String>,
}

impl App {
    /// 渲染文件哈希校验对话框
    pub fn render_hash_verify_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_hash_verify_dialog {
            return;
        }

        let mut should_close = false;

        egui::Window::new("文件哈希校验 (SHA-256)")
            .resizable(true)
            .default_width(600.0)
            .default_height(360.0)
            .show(ui.ctx(), |ui| {
                ui.label("计算文件的 SHA-256，并可与期望值比对（核对下载完整性）。");
                ui.add_space(10.0);

                // 文件路径
                ui.horizontal(|ui| {
                    ui.label("文件:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.hash_verify_file_path)
                            .hint_text("输入或选择文件路径")
                            .desired_width(380.0),
                    );
                    let can_browse = !self.hash_verify_loading;
                    if ui.add_enabled(can_browse, egui::Button::new("浏览...")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("系统镜像", &["wim", "esd", "swm", "gho", "ghs", "iso"])
                            .add_filter("所有文件", &["*"])
                            .pick_file()
                        {
                            self.hash_verify_file_path = path.to_string_lossy().to_string();
                            self.hash_verify_result = None;
                        }
                    }
                });

                // 期望哈希（可选）
                ui.horizontal(|ui| {
                    ui.label("期望哈希:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.hash_verify_expected)
                            .hint_text("可选：粘贴官方 SHA-256 以比对")
                            .desired_width(380.0),
                    );
                });

                ui.add_space(15.0);

                // 计算按钮 + 进度
                ui.horizontal(|ui| {
                    let can_run =
                        !self.hash_verify_file_path.is_empty() && !self.hash_verify_loading;
                    if ui.add_enabled(can_run, egui::Button::new("计算 SHA-256")).clicked() {
                        self.start_hash_verify();
                    }
                    if self.hash_verify_loading {
                        ui.add_space(10.0);
                        ui.spinner();
                        let pct = self.hash_verify_progress.unwrap_or(0);
                        ui.label(format!("{}%", pct));
                    }
                });

                if self.hash_verify_loading {
                    ui.add_space(10.0);
                    let progress = self.hash_verify_progress.unwrap_or(0) as f32 / 100.0;
                    ui.add(egui::ProgressBar::new(progress).show_percentage());
                }

                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);

                // 结果
                if let Some(result) = self.hash_verify_result.clone() {
                    Self::render_hash_result(ui, &result);
                } else if !self.hash_verify_loading {
                    ui.colored_label(egui::Color32::GRAY, "请选择文件并点击「计算 SHA-256」");
                }

                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_hash_verify_dialog = false;
        }
    }

    /// 渲染哈希结果
    fn render_hash_result(ui: &mut egui::Ui, result: &HashVerifyResult) {
        ui.horizontal(|ui| {
            ui.label("文件:");
            ui.label(&result.file_path);
        });
        ui.horizontal(|ui| {
            ui.label("大小:");
            ui.label(Self::format_hash_file_size(result.file_size));
        });

        if let Some(ref err) = result.error {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::from_rgb(255, 80, 80), format!("{}", err));
            return;
        }

        ui.add_space(8.0);
        ui.label("SHA-256:");
        // 等宽显示，egui 标签默认可选中，便于复制
        ui.monospace(result.sha256.as_str());

        // 与期望值比对结果
        match result.matched {
            Some(true) => {
                ui.add_space(6.0);
                ui.colored_label(egui::Color32::from_rgb(0, 200, 0), "与期望哈希一致");
            }
            Some(false) => {
                ui.add_space(6.0);
                ui.colored_label(
                    egui::Color32::from_rgb(255, 80, 80),
                    "与期望哈希不一致（文件可能损坏或被篡改）",
                );
            }
            None => {
                ui.add_space(6.0);
                ui.colored_label(egui::Color32::GRAY, "（未提供期望哈希，仅展示计算值）");
            }
        }
    }

    fn format_hash_file_size(size: u64) -> String {
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

    /// 开始计算哈希（后台线程）
    fn start_hash_verify(&mut self) {
        if self.hash_verify_loading {
            return;
        }
        let file_path = self.hash_verify_file_path.clone();
        if file_path.is_empty() {
            return;
        }
        if !std::path::Path::new(&file_path).exists() {
            self.hash_verify_result = Some(HashVerifyResult {
                file_path,
                error: Some("文件不存在".to_string()),
                ..Default::default()
            });
            return;
        }

        let expected = self.hash_verify_expected.clone();
        self.hash_verify_loading = true;
        self.hash_verify_result = None;
        self.hash_verify_progress = Some(0);

        let (progress_tx, progress_rx) = mpsc::channel::<u8>();
        self.hash_verify_progress_rx = Some(progress_rx);
        let (result_tx, result_rx) = mpsc::channel::<HashVerifyResult>();
        self.hash_verify_result_rx = Some(result_rx);

        std::thread::spawn(move || {
            let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
            let result = match lr_core::hash::sha256_file(&file_path, |read| {
                let pct = if file_size > 0 {
                    (read.min(file_size) * 100 / file_size) as u8
                } else {
                    0
                };
                let _ = progress_tx.send(pct);
            }) {
                Ok(sha) => {
                    let matched = if expected.trim().is_empty() {
                        None
                    } else {
                        Some(lr_core::hash::hash_matches(&sha, &expected))
                    };
                    HashVerifyResult {
                        file_path,
                        file_size,
                        sha256: sha,
                        expected,
                        matched,
                        error: None,
                    }
                }
                Err(e) => HashVerifyResult {
                    file_path,
                    file_size,
                    error: Some(format!("读取/计算失败: {}", e)),
                    ..Default::default()
                },
            };
            let _ = result_tx.send(result);
        });
    }

    /// 轮询哈希计算状态（在主循环中调用）
    pub fn check_hash_verify_status(&mut self) {
        if let Some(ref rx) = self.hash_verify_progress_rx {
            while let Ok(p) = rx.try_recv() {
                self.hash_verify_progress = Some(p);
            }
        }
        if let Some(ref rx) = self.hash_verify_result_rx {
            if let Ok(result) = rx.try_recv() {
                self.hash_verify_result = Some(result);
                self.hash_verify_loading = false;
                self.hash_verify_progress = None;
                self.hash_verify_progress_rx = None;
                self.hash_verify_result_rx = None;
            }
        }
    }
}
