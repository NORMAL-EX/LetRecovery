use egui;
use crate::tr;
use crate::app::App;
use super::super::network::get_detailed_network_info;
use super::super::network::reset_network;

impl App {
    /// 渲染网络信息对话框
    pub fn render_network_info_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_network_info_dialog {
            return;
        }

        egui::Window::new(tr!("本机网络信息"))
            .open(&mut self.show_network_info_dialog)
            .resizable(true)
            .default_width(500.0)
            .default_height(400.0)
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(ref adapters) = self.network_info_cache {
                        if adapters.is_empty() {
                            ui.label(tr!("未检测到网络适配器"));
                        } else {
                            for (i, adapter) in adapters.iter().enumerate() {
                                egui::CollapsingHeader::new(tr!(
                                    "适配器 {}: {}",
                                    i + 1,
                                    adapter.description
                                ))
                                .default_open(true)
                                .show(ui, |ui| {
                                    egui::Grid::new(format!("net_info_grid_{}", i))
                                        .num_columns(2)
                                        .spacing([20.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.label(tr!("名称:"));
                                            ui.label(&adapter.name);
                                            ui.end_row();

                                            ui.label(tr!("描述:"));
                                            ui.label(&adapter.description);
                                            ui.end_row();

                                            if !adapter.adapter_type.is_empty() {
                                                ui.label(tr!("类型:"));
                                                ui.label(&adapter.adapter_type);
                                                ui.end_row();
                                            }

                                            if !adapter.mac_address.is_empty() {
                                                ui.label(tr!("MAC 地址:"));
                                                ui.label(&adapter.mac_address);
                                                ui.end_row();
                                            }

                                            if !adapter.ip_addresses.is_empty() {
                                                ui.label(tr!("IP 地址:"));
                                                for ip in &adapter.ip_addresses {
                                                    ui.label(ip);
                                                    ui.end_row();
                                                    ui.label("");
                                                }
                                            }

                                            if !adapter.status.is_empty() {
                                                ui.label(tr!("状态:"));
                                                ui.label(&adapter.status);
                                                ui.end_row();
                                            }

                                            if adapter.speed > 0 {
                                                ui.label(tr!("速度:"));
                                                let speed_mbps = adapter.speed / 1_000_000;
                                                ui.label(format!("{} Mbps", speed_mbps));
                                                ui.end_row();
                                            }
                                        });
                                });
                                ui.add_space(10.0);
                            }
                        }
                    } else {
                        ui.spinner();
                        ui.label(tr!("正在获取网络信息..."));
                    }
                });
            });
    }

    /// 初始化网络信息对话框
    pub fn init_network_info_dialog(&mut self) {
        self.show_network_info_dialog = true;
        self.network_info_cache = Some(get_detailed_network_info());
    }

    /// 渲染重置网络确认对话框
    pub fn render_reset_network_confirm_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_reset_network_confirm_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_reset = false;

        egui::Window::new(tr!("确认重置网络设置"))
            .resizable(false)
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(6.0);
                ui.label(tr!("此操作将执行以下命令重置网络设置："));
                ui.add_space(5.0);

                ui.add(
                    egui::Label::new(egui::RichText::new(
                        "• netsh winsock reset\n\
                         • netsh int ip reset\n\
                         • ipconfig /flushdns\n\
                         • netsh advfirewall reset",
                    )
                    .monospace()
                    .size(12.0)),
                );

                ui.add_space(10.0);
                ui.label(tr!("重置后可能需要重新配置网络连接。"));
                ui.add_space(15.0);

                ui.horizontal(|ui| {
                    if ui.button(tr!("确认重置")).clicked() {
                        do_reset = true;
                        should_close = true;
                    }
                    if ui.button(tr!("取消")).clicked() {
                        should_close = true;
                    }
                });
            });

        if do_reset {
            self.do_reset_network();
        }

        if should_close {
            self.show_reset_network_confirm_dialog = false;
        }
    }

    /// 执行网络重置
    pub fn do_reset_network(&mut self) {
        let (success_count, fail_count) = reset_network();

        self.tool_message = tr!(
            "网络重置完成: 成功 {} 个命令, 失败 {} 个命令",
            success_count, fail_count
        );

        if success_count > 0 {
            self.tool_message.push_str(&tr!("\n建议重启计算机以完成网络重置。"));
        }
    }
}
