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

                // 版本信息（编译时按日期自动生成，见 build.rs）
                ui.horizontal(|ui| {
                    ui.label(tr!("版本:"));
                    ui.strong(env!("BUILD_VERSION"));
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
                                        // 备份名/描述在启动时按当时语言生成并缓存，切换语言后需重新生成，
                                        // 否则会停留在旧语言（其余界面文本每帧用 tr! 渲染会自动刷新）。
                                        self.backup_name = tr!(
                                            "系统备份_{}",
                                            chrono::Local::now().format("%Y%m%d_%H%M%S")
                                        );
                                        self.backup_description =
                                            tr!("使用 LetRecovery 创建的系统备份");
                                        // 远程配置错误信息在加载时按当时语言生成并缓存于
                                        // remote_config.error，切换语言后重新拉取，使其也用新语言显示。
                                        self.start_remote_config_loading();
                                        // 窗口标题在启动时设定，切换语言时通过 viewport 命令同步更新。
                                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(
                                            tr!("LetRecovery - Windows系统一键重装工具"),
                                        ));
                                        // 立即重绘，使整个界面即时应用新语言（否则需下一次交互才刷新）
                                        ui.ctx().request_repaint();
                                    }
                                }
                            }
                        });
                    
                    // 刷新语言列表按钮
                    if ui.button(tr!("刷新")).on_hover_text(tr!("刷新语言列表")).clicked() {
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
                        tr!("反馈软件问题时，请将日志（log）等必要信息一并提供给开发者，"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("以便更快定位与解决问题。开关在下次启动时生效。"),
                    );
                });

                // 提供一个入口便于用户找到并发送日志
                if self.app_config.log_enabled {
                    ui.add_space(8.0);
                    let log_dir = LogManager::get_log_dir();
                    if ui.button(format!("{}", tr!("打开日志目录"))).clicked() {
                        if log_dir.exists() {
                            #[cfg(windows)]
                            {
                                let _ = std::process::Command::new("explorer")
                                    .arg(&log_dir)
                                    .spawn();
                            }
                        }
                    }
                }

                ui.add_space(10.0);
                ui.separator();

                // 镜像引擎设置
                ui.add_space(10.0);
                ui.heading(tr!("镜像引擎"));
                ui.add_space(10.0);

                let current_engine = self.app_config.wim_engine;
                let current_label = if current_engine == 1 {
                    tr!("wimgapi（系统原生 API）")
                } else {
                    tr!("libwim（内置，默认）")
                };
                ui.horizontal(|ui| {
                    ui.label(tr!("WIM 引擎:"));
                    egui::ComboBox::from_id_salt("wim_engine_selector")
                        .selected_text(current_label)
                        .width(280.0)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(current_engine == 0, tr!("libwim（内置，默认）"))
                                .clicked()
                                && current_engine != 0
                            {
                                self.app_config.set_wim_engine(0);
                            }
                            if ui
                                .selectable_label(current_engine == 1, tr!("wimgapi（系统原生 API）"))
                                .clicked()
                                && current_engine != 1
                            {
                                self.app_config.set_wim_engine(1);
                            }
                        });
                });

                ui.add_space(5.0);
                ui.indent("wim_engine_desc", |ui| {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("镜像释放/备份使用的底层引擎。libwim 为内置默认；wimgapi 为 Windows 原生接口。"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("切换后正常系统端与 PE 端均使用该引擎；若 wimgapi 不可用会自动回退到 libwim。"),
                    );
                });

                ui.add_space(10.0);
                ui.separator();

                // 高级选项（总开关，存 config.json；小白勿开）
                ui.add_space(10.0);
                ui.heading(tr!("高级选项"));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let mut adv = self.app_config.enable_advanced_options;
                    if ui.checkbox(&mut adv, tr!("启用高级选项")).changed() {
                        self.app_config.set_advanced_options(adv);
                    }
                });
                ui.add_space(5.0);
                ui.indent("advanced_options_desc", |ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 80, 80),
                        tr!("⚠ 面向高级用户，小白请勿开启，设置不当可能导致无法开机。"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("开启后解锁：安装 XP 时可选 UEFI 引导（供 UEFI 化魔改镜像）、"),
                    );
                    ui.colored_label(
                        egui::Color32::GRAY,
                        tr!("系统安装页「运行 Diskpart 脚本」、自定义修复引导脚本 bin\\repair_boot.txt。"),
                    );
                });

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