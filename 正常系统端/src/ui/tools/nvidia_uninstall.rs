//! 英伟达显卡驱动卸载对话框模块
//!
//! 提供英伟达显卡驱动卸载的UI界面

use egui;
use std::sync::mpsc;

use crate::app::App;
use crate::core::nvidia_driver::{
    beautify_gpu_name, get_system_hardware_summary,
    uninstall_nvidia_drivers_offline, uninstall_nvidia_drivers_online,
};
use super::types::{NvidiaUninstallResult, WindowsPartitionInfo};

impl App {
    /// 渲染英伟达驱动卸载对话框
    pub fn render_nvidia_uninstall_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_nvidia_uninstall_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_uninstall = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;
        let is_pe = self.is_pe_environment();

        egui::Window::new("英伟达显卡驱动卸载")
            .resizable(true)
            .default_width(600.0)
            .default_height(500.0)
            .show(ui.ctx(), |ui| {
                ui.label("此工具用于卸载系统中的英伟达(NVIDIA)显卡驱动");
                ui.add_space(10.0);

                // 硬件信息显示区域
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(40, 40, 40))
                    .inner_margin(10.0)
                    .corner_radius(5.0)
                    .show(ui, |ui| {
                        if self.nvidia_uninstall_hardware_loading {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("正在加载硬件信息...");
                            });
                        } else if let Some(ref summary) = self.nvidia_uninstall_hardware_summary {
                            // 显示显卡信息
                            for (i, gpu) in summary.gpu_devices.iter().enumerate() {
                                let display_name = if !gpu.friendly_name.is_empty() {
                                    beautify_gpu_name(&gpu.friendly_name)
                                } else {
                                    beautify_gpu_name(&gpu.name)
                                };

                                ui.horizontal(|ui| {
                                    ui.label(format!("显卡{}型号:", i + 1));
                                    if gpu.is_nvidia {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(118, 185, 0),
                                            &display_name,
                                        );
                                        ui.colored_label(
                                            egui::Color32::from_rgb(118, 185, 0),
                                            "(NVIDIA)",
                                        );
                                    } else {
                                        ui.label(&display_name);
                                    }
                                });
                                
                                ui.horizontal(|ui| {
                                    ui.label(format!("显卡{}硬件ID:", i + 1));
                                    ui.monospace(&gpu.hardware_id);
                                });
                            }

                            if summary.gpu_devices.is_empty() {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    "未检测到显卡设备",
                                );
                            }

                            // 分隔线
                            ui.add_space(5.0);
                            ui.separator();
                            ui.add_space(5.0);

                            // CPU 信息
                            ui.label(&summary.cpu_name);

                            // 分隔线
                            ui.add_space(5.0);
                            ui.separator();
                            ui.add_space(5.0);

                            // 内存信息
                            let total_gb = summary.memory_size as f64 / (1024.0 * 1024.0 * 1024.0);
                            let avail_gb = summary.memory_available as f64 / (1024.0 * 1024.0 * 1024.0);
                            ui.label(format!(
                                "内存大小: {:.0} GB ({:.1} GB可用)",
                                total_gb.ceil(),
                                avail_gb
                            ));
                        } else {
                            ui.label("无法获取硬件信息");
                        }
                    });

                ui.add_space(15.0);

                // 目标系统选择
                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测Windows分区...");
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label("请选择Windows系统:");

                        let current_text = self
                            .nvidia_uninstall_target
                            .as_ref()
                            .map(|letter| {
                                if letter == "__CURRENT__" {
                                    "当前系统".to_string()
                                } else {
                                    format_partition_display(&windows_partitions, letter)
                                }
                            })
                            .unwrap_or_else(|| "请选择".to_string());

                        egui::ComboBox::from_id_salt("nvidia_uninstall_partition")
                            .selected_text(current_text)
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                // 非PE环境显示"当前系统"选项
                                if !is_pe {
                                    ui.selectable_value(
                                        &mut self.nvidia_uninstall_target,
                                        Some("__CURRENT__".to_string()),
                                        "当前系统",
                                    );
                                    if !windows_partitions.is_empty() {
                                        ui.separator();
                                    }
                                }

                                // 离线分区选项
                                for partition in &windows_partitions {
                                    let display = format!(
                                        "{} [{}] [{}]",
                                        partition.letter,
                                        partition.windows_version,
                                        partition.architecture
                                    );
                                    ui.selectable_value(
                                        &mut self.nvidia_uninstall_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });
                }

                ui.add_space(15.0);

                // 状态消息
                if !self.nvidia_uninstall_message.is_empty() {
                    let color = if self.nvidia_uninstall_message.contains("成功") {
                        egui::Color32::from_rgb(0, 180, 0)
                    } else if self.nvidia_uninstall_message.contains("失败")
                        || self.nvidia_uninstall_message.contains("错误")
                    {
                        egui::Color32::from_rgb(255, 80, 80)
                    } else {
                        egui::Color32::GRAY
                    };
                    ui.colored_label(color, &self.nvidia_uninstall_message);
                    ui.add_space(10.0);
                }

                // 警告信息
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(60, 40, 20))
                    .inner_margin(10.0)
                    .corner_radius(5.0)
                    .show(ui, |ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 200, 100),
                            "⚠️ 注意事项:",
                        );
                        ui.label("1. 卸载驱动后可能需要重启系统");
                        ui.label("2. 卸载后显示可能切换到基本显示适配器");
                        ui.label("3. 建议在卸载前备份重要数据");
                        if is_pe {
                            ui.label("4. 当前在PE环境中，将清理离线系统的英伟达驱动文件");
                        }
                    });

                ui.add_space(15.0);

                // 按钮区域
                ui.horizontal(|ui| {
                    if self.nvidia_uninstall_loading {
                        ui.spinner();
                        ui.label("正在卸载驱动，请稍候...");
                    } else {
                        // 检查是否有英伟达设备
                        let has_nvidia = self
                            .nvidia_uninstall_hardware_summary
                            .as_ref()
                            .map(|s| s.gpu_devices.iter().any(|g| g.is_nvidia))
                            .unwrap_or(false);

                        let can_uninstall = self.nvidia_uninstall_target.is_some()
                            && !is_loading_partitions
                            && !self.nvidia_uninstall_hardware_loading;

                        // 如果没有检测到英伟达设备，显示警告但仍允许操作（可能是离线系统）
                        if !has_nvidia && !is_pe {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "⚠️ 当前系统未检测到英伟达显卡",
                            );
                            ui.add_space(10.0);
                        }

                        if ui
                            .add_enabled(can_uninstall, egui::Button::new("🗑 开始卸载"))
                            .clicked()
                        {
                            do_uninstall = true;
                        }

                        if ui.button("刷新").clicked() {
                            self.start_load_nvidia_hardware_summary();
                            self.refresh_windows_partitions_cache();
                        }

                        if ui.button("关闭").clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_uninstall {
            self.start_nvidia_uninstall();
        }

        if should_close {
            self.show_nvidia_uninstall_dialog = false;
        }
    }

    /// 启动后台加载硬件摘要信息
    pub fn start_load_nvidia_hardware_summary(&mut self) {
        if self.nvidia_uninstall_hardware_loading {
            return;
        }

        self.nvidia_uninstall_hardware_loading = true;
        self.nvidia_uninstall_hardware_summary = None;

        let (tx, rx) = mpsc::channel();
        self.nvidia_uninstall_hardware_rx = Some(rx);

        std::thread::spawn(move || {
            let summary = get_system_hardware_summary().unwrap_or_default();
            let _ = tx.send(summary);
        });
    }

    /// 启动后台卸载英伟达驱动
    fn start_nvidia_uninstall(&mut self) {
        if self.nvidia_uninstall_loading {
            return;
        }

        let target = match &self.nvidia_uninstall_target {
            Some(t) => t.clone(),
            None => {
                self.nvidia_uninstall_message = "请先选择目标系统".to_string();
                return;
            }
        };

        self.nvidia_uninstall_loading = true;
        self.nvidia_uninstall_message = "正在卸载英伟达驱动...".to_string();

        let (tx, rx) = mpsc::channel();
        self.nvidia_uninstall_rx = Some(rx);

        let is_current = target == "__CURRENT__";

        std::thread::spawn(move || {
            let result = if is_current {
                // 在线卸载
                match uninstall_nvidia_drivers_online() {
                    Ok(r) => NvidiaUninstallResult {
                        success: r.success,
                        message: r.message,
                        needs_reboot: r.needs_reboot,
                        uninstalled_count: r.uninstalled_count,
                        failed_count: r.failed_count,
                    },
                    Err(e) => NvidiaUninstallResult {
                        success: false,
                        message: format!("卸载失败: {}", e),
                        ..Default::default()
                    },
                }
            } else {
                // 离线卸载
                match uninstall_nvidia_drivers_offline(&target) {
                    Ok(r) => NvidiaUninstallResult {
                        success: r.success,
                        message: r.message,
                        needs_reboot: r.needs_reboot,
                        uninstalled_count: r.uninstalled_count,
                        failed_count: r.failed_count,
                    },
                    Err(e) => NvidiaUninstallResult {
                        success: false,
                        message: format!("卸载失败: {}", e),
                        ..Default::default()
                    },
                }
            };

            let _ = tx.send(result);
        });
    }

    /// 检查英伟达驱动卸载结果
    pub fn check_nvidia_uninstall_result(&mut self) {
        // 检查硬件信息加载结果
        if let Some(ref rx) = self.nvidia_uninstall_hardware_rx {
            if let Ok(summary) = rx.try_recv() {
                self.nvidia_uninstall_hardware_summary = Some(summary);
                self.nvidia_uninstall_hardware_loading = false;
                self.nvidia_uninstall_hardware_rx = None;
            }
        }

        // 检查卸载结果
        if let Some(ref rx) = self.nvidia_uninstall_rx {
            if let Ok(result) = rx.try_recv() {
                self.nvidia_uninstall_message = if result.success {
                    if result.needs_reboot {
                        format!("{}，建议重启系统", result.message)
                    } else {
                        result.message
                    }
                } else {
                    result.message
                };
                self.nvidia_uninstall_loading = false;
                self.nvidia_uninstall_rx = None;
                
                // 刷新硬件信息
                if result.success {
                    self.start_load_nvidia_hardware_summary();
                }
            }
        }
    }
}

/// 格式化分区显示文本
fn format_partition_display(partitions: &[WindowsPartitionInfo], letter: &str) -> String {
    partitions
        .iter()
        .find(|p| p.letter == letter)
        .map(|p| format!("{} [{}] [{}]", p.letter, p.windows_version, p.architecture))
        .unwrap_or_else(|| letter.to_string())
}
