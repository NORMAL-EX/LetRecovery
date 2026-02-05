use egui;

use crate::app::App;
use crate::utils::i18n::{self};
use crate::utils::logger::LogManager;
use crate::tr;

impl App {
    pub fn show_about(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();

        egui::ScrollArea::vertical()
            .max_height(available_height)
            .show(ui, |ui| {
                ui.heading(tr!("关于 LetRecovery"));
                ui.separator();

                ui.add_space(20.0);

                // 版本信息
                ui.horizontal(|ui| {
                    ui.label(tr!("版本:"));
                    ui.strong("v2026.2.6");
                });

                ui.add_space(15.0);
                
                // 语言设置
                ui.separator();
                ui.add_space(10.0);
                ui.heading(tr!("语言设置"));
                ui.add_space(10.0);
                
                // 获取可用语言列表
                let available_languages = i18n::get_available_languages();
                let current_language = self.app_config.language.clone();
                
                ui.horizontal(|ui| {
                    ui.label(tr!("界面语言:"));
                    
                    // 查找当前语言的显示名称
                    let current_display = available_languages
                        .iter()
                        .find(|l| l.code == current_language)
                        .map(|l| l.display_name.as_str())
                        .unwrap_or("简体中文 - 中华人民共和国");
                    
                    egui::ComboBox::from_id_salt("language_selector")
                        .selected_text(current_display)
                        .width(280.0)
                        .show_ui(ui, |ui| {
                            for lang in &available_languages {
                                let is_selected = lang.code == current_language;
                                if ui.selectable_label(is_selected, &lang.display_name).clicked() {
                                    if lang.code != current_language {
                                        self.app_config.set_language(&lang.code);
                                    }
                                }
                            }
                        });
                    
                    // 刷新语言列表按钮
                    if ui.button("🔄").on_hover_text(tr!("刷新语言列表")).clicked() {
                        i18n::refresh_available_languages();
                    }
                });
                
                // 显示当前语言作者信息
                if let Some(lang_info) = available_languages.iter().find(|l| l.code == current_language) {
                    if lang_info.code != "zh-CN" {
                        ui.add_space(5.0);
                        ui.indent("lang_author", |ui| {
                            ui.colored_label(
                                egui::Color32::GRAY,
                                format!("{}: {}", tr!("翻译作者"), lang_info.author),
                            );
                        });
                    }
                }
                
                ui.add_space(5.0);
                ui.indent("lang_desc", |ui| {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("将语言文件放入程序目录的 lang 文件夹中，"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("然后点击刷新按钮即可添加新语言。"),
                    );
                });

                ui.add_space(10.0);
                ui.separator();
                
                // 小白模式设置
                ui.add_space(10.0);
                ui.heading(tr!("模式设置"));
                ui.add_space(10.0);
                
                let is_pe = self.system_info.as_ref()
                    .map(|info| info.is_pe_environment)
                    .unwrap_or(false);
                
                ui.horizontal(|ui| {
                    let mut easy_mode = self.app_config.easy_mode_enabled;
                    
                    ui.add_enabled_ui(!is_pe, |ui| {
                        if ui.checkbox(&mut easy_mode, tr!("启用小白模式")).changed() {
                            self.app_config.set_easy_mode(easy_mode);
                        }
                    });
                    
                    if is_pe {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 165, 0),
                            tr!("(PE环境下不可用)"),
                        );
                    }
                });
                
                ui.add_space(5.0);
                ui.indent("easy_mode_desc", |ui| {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("小白模式提供简化的系统重装界面，自动应用推荐设置，"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("适合不熟悉系统重装操作的用户。"),
                    );
                });
                
                ui.add_space(10.0);
                ui.separator();
                
                // 日志设置
                ui.add_space(10.0);
                ui.heading(tr!("日志设置"));
                ui.add_space(10.0);
                
                // 日志开关
                ui.horizontal(|ui| {
                    let mut log_enabled = self.app_config.log_enabled;
                    if ui.checkbox(&mut log_enabled, tr!("启用日志记录")).changed() {
                        self.app_config.set_log_enabled(log_enabled);
                    }
                });
                
                ui.add_space(5.0);
                ui.indent("log_desc", |ui| {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("日志文件保存在程序目录的 log 文件夹中，"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("用于故障排查和问题诊断。关闭后将在下次启动时生效。"),
                    );
                });
                
                // 日志目录信息
                if self.app_config.log_enabled {
                    ui.add_space(8.0);
                    
                    let log_dir = LogManager::get_log_dir();
                    let log_size = LogManager::get_log_dir_size();
                    let size_str = LogManager::format_size(log_size);
                    
                    ui.horizontal(|ui| {
                        ui.label(tr!("日志目录:"));
                        ui.monospace(log_dir.display().to_string());
                    });
                    
                    ui.horizontal(|ui| {
                        ui.label(tr!("日志大小:"));
                        ui.monospace(&size_str);
                        
                        ui.add_space(20.0);
                        
                        // 打开日志目录按钮
                        if ui.button(format!("📂 {}", tr!("打开日志目录"))).clicked() {
                            if log_dir.exists() {
                                #[cfg(windows)]
                                {
                                    let _ = std::process::Command::new("explorer")
                                        .arg(&log_dir)
                                        .spawn();
                                }
                            }
                        }
                        
                        // 清理日志按钮
                        if ui.button(format!("🗑 {}", tr!("清理旧日志"))).clicked() {
                            if let Err(e) = LogManager::cleanup_old_logs(self.app_config.log_retention_days) {
                                log::warn!("清理日志失败: {}", e);
                            } else {
                                log::info!("日志清理完成");
                            }
                        }
                    });
                    
                    // 日志保留天数设置
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.label(tr!("日志保留天数:"));
                        let mut days = self.app_config.log_retention_days;
                        let slider = egui::Slider::new(&mut days, 1..=30)
                            .suffix(format!(" {}", tr!("天")));
                        if ui.add(slider).changed() {
                            self.app_config.set_log_retention_days(days);
                        }
                    });
                }

                ui.add_space(10.0);
                ui.separator();

                ui.add_space(15.0);

                // 版权信息
                ui.label(tr!("版权:"));
                ui.indent("copyright", |ui| {
                    ui.label("\u{00A9} 2026-present Cloud-PE Dev.");
                    ui.label("\u{00A9} 2026-present NORMAL-EX.");
                });

                ui.add_space(15.0);

                // 开源链接
                ui.horizontal(|ui| {
                    ui.label(tr!("开源地址:"));
                    ui.hyperlink_to(
                        "https://github.com/NORMAL-EX/LetRecovery",
                        "https://github.com/NORMAL-EX/LetRecovery",
                    );
                });

                ui.add_space(10.0);

                // 许可证
                ui.horizontal(|ui| {
                    ui.label(tr!("许可证:"));
                    ui.strong("PolyForm Noncommercial License 1.0.0");
                });

                ui.add_space(20.0);
                ui.separator();

                // 免费声明
                ui.heading(tr!("免费声明"));
                ui.add_space(10.0);

                ui.colored_label(
                    egui::Color32::from_rgb(0, 200, 83),
                    format!("✓ {}", tr!("本软件完全免费，禁止任何形式的倒卖行为！")),
                );

                ui.add_space(8.0);

                ui.label(tr!("如果您是通过付费渠道获取本软件，您已被骗，请立即举报并申请退款。"));

                ui.add_space(15.0);

                // 使用条款
                ui.heading(tr!("使用条款"));
                ui.add_space(10.0);

                ui.colored_label(egui::Color32::from_rgb(100, 181, 246), format!("{}：", tr!("允许")));
                ui.indent("allowed", |ui| {
                    ui.label(format!("• {}", tr!("个人学习、研究和非盈利使用")));
                    ui.label(format!("• {}", tr!("修改源代码并用于非盈利用途")));
                    ui.label(format!("• {}", tr!("在注明出处的前提下进行非商业性质的分发")));
                });

                ui.add_space(10.0);

                ui.colored_label(egui::Color32::from_rgb(239, 83, 80), format!("{}：", tr!("禁止")));
                ui.indent("forbidden", |ui| {
                    ui.label(format!("• {}", tr!("将本软件或其源代码用于任何商业/盈利用途")));
                    ui.label(format!("• {}", tr!("销售、倒卖本软件或其衍生作品")));
                    ui.label(format!("• {}", tr!("将本软件整合到商业产品或服务中")));
                    ui.label(format!("• {}", tr!("个人利用本软件或其代码进行盈利活动")));
                });

                ui.add_space(20.0);
                ui.separator();

                // 致谢
                ui.heading(tr!("致谢"));

                ui.add_space(10.0);

                ui.label(format!("• {}", tr!("部分系统镜像及 PE 下载服务由 Cloud-PE 云盘提供")));
                ui.label(format!("• {}", tr!("感谢 电脑病毒爱好者 提供 WinPE")));

                ui.add_space(30.0);
                ui.separator();

                // 说明
                ui.add_space(10.0);
                ui.colored_label(
                    egui::Color32::GRAY,
                    tr!("LetRecovery 是一款免费开源的 Windows 系统重装工具，"),
                );
                ui.colored_label(
                    egui::Color32::GRAY,
                    tr!("支持本地镜像安装、在线下载安装、系统备份等功能。"),
                );

                ui.add_space(20.0);
            });
    }
}