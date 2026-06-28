use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::common::get_message_color;

impl App {
    // ==================== 时间同步对话框 ====================
    
    /// 渲染时间同步对话框
    pub fn render_time_sync_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_time_sync_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_sync = false;

        egui::Window::new(tr!("系统时间校准"))
            .resizable(false)
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(6.0);
                ui.label(tr!("是否立即网络同步本机的时间到北京时间？"));
                ui.add_space(10.0);

                ui.label(egui::RichText::new(tr!("将从以下NTP服务器获取时间：")).small());
                ui.label(egui::RichText::new("• ntp.aliyun.com\n• ntp.tencent.com\n• cn.ntp.org.cn").monospace().small());
                
                ui.add_space(15.0);

                // 显示状态消息
                if !self.time_sync_message.is_empty() {
                    let color = get_message_color(&self.time_sync_message);
                    ui.colored_label(color, &self.time_sync_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.time_sync_loading {
                        ui.spinner();
                        ui.label(tr!("正在同步时间..."));
                    } else {
                        if ui.button(tr!("确定")).clicked() {
                            do_sync = true;
                        }
                        if ui.button(tr!("取消")).clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_sync {
            self.start_time_sync();
        }

        if should_close {
            self.show_time_sync_dialog = false;
        }
    }

    /// 启动后台时间同步
    fn start_time_sync(&mut self) {
        if self.time_sync_loading {
            return;
        }

        self.time_sync_loading = true;
        self.time_sync_message = tr!("正在连接NTP服务器...");

        let (tx, rx) = mpsc::channel();
        self.time_sync_rx = Some(rx);

        std::thread::spawn(move || {
            let result = super::super::time_sync::sync_time_to_beijing();
            let _ = tx.send(result);
        });
    }
}
