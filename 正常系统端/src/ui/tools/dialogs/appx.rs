use egui;
use std::collections::HashSet;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::super::appx::{get_appx_packages, remove_appx_packages};
use super::common::{format_partition_display, get_message_color};

impl App {
    /// 渲染移除APPX对话框
    pub fn render_remove_appx_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_remove_appx_dialog {
            return;
        }

        let mut should_close = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;
        let is_pe = self.is_pe_environment();

        egui::Window::new(tr!("移除APPX应用"))
            .resizable(true)
            .default_width(550.0)
            .default_height(450.0)
            .show(ui.ctx(), |ui| {
                if is_pe {
                    ui.label(tr!("移除离线系统中预装的 Microsoft Store 应用"));
                } else {
                    ui.label(tr!("移除当前系统或离线系统中的 Microsoft Store 应用"));
                }
                ui.add_space(10.0);

                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测Windows分区..."));
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(tr!("目标系统:"));

                        let current_text = self
                            .remove_appx_target
                            .as_ref()
                            .map(|letter| {
                                if letter == "__CURRENT__" {
                                    tr!("当前系统")
                                } else {
                                    format_partition_display(&windows_partitions, letter)
                                }
                            })
                            .unwrap_or_else(|| tr!("请选择"));

                        let old_target = self.remove_appx_target.clone();

                        egui::ComboBox::from_id_salt("remove_appx_partition")
                            .selected_text(current_text)
                            .show_ui(ui, |ui| {
                                // 非PE环境显示"当前系统"选项
                                if !is_pe {
                                    ui.selectable_value(
                                        &mut self.remove_appx_target,
                                        Some("__CURRENT__".to_string()),
                                        tr!("当前系统"),
                                    );
                                    ui.separator();
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
                                        &mut self.remove_appx_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });

                        // 分区改变时重新加载APPX列表
                        if old_target != self.remove_appx_target && self.remove_appx_target.is_some()
                        {
                            self.start_load_appx_list();
                        }
                    });
                }

                ui.add_space(10.0);

                // APPX列表
                if self.remove_appx_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在处理..."));
                    });
                } else if !self.remove_appx_list.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.button(tr!("全选")).clicked() {
                            for pkg in &self.remove_appx_list {
                                self.remove_appx_selected
                                    .insert(pkg.package_name.clone());
                            }
                        }
                        if ui.button(tr!("反选")).clicked() {
                            let current: HashSet<_> = self.remove_appx_selected.clone();
                            self.remove_appx_selected.clear();
                            for pkg in &self.remove_appx_list {
                                if !current.contains(&pkg.package_name) {
                                    self.remove_appx_selected
                                        .insert(pkg.package_name.clone());
                                }
                            }
                        }
                        ui.label(tr!("已选择 {} 个应用", self.remove_appx_selected.len()));
                    });

                    ui.add_space(5.0);

                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for pkg in &self.remove_appx_list {
                                let mut selected =
                                    self.remove_appx_selected.contains(&pkg.package_name);
                                if ui.checkbox(&mut selected, &pkg.display_name).changed() {
                                    if selected {
                                        self.remove_appx_selected
                                            .insert(pkg.package_name.clone());
                                    } else {
                                        self.remove_appx_selected.remove(&pkg.package_name);
                                    }
                                }
                            }
                        });
                } else if self.remove_appx_target.is_some() && !is_loading_partitions {
                    ui.label(tr!("未找到可移除的应用，或请先点击刷新列表按钮"));
                }

                ui.add_space(10.0);

                // 状态消息
                if !self.remove_appx_message.is_empty() {
                    let color = get_message_color(&self.remove_appx_message);
                    ui.colored_label(color, &self.remove_appx_message);
                }

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    let can_remove = !self.remove_appx_selected.is_empty()
                        && !self.remove_appx_loading
                        && self.remove_appx_target.is_some();

                    if ui
                        .add_enabled(can_remove, egui::Button::new(tr!("移除选中应用")))
                        .clicked()
                    {
                        self.start_remove_appx();
                    }

                    let can_refresh = self.remove_appx_target.is_some()
                        && !self.remove_appx_loading
                        && !is_loading_partitions;
                    if ui.add_enabled(can_refresh, egui::Button::new(tr!("刷新列表"))).clicked() {
                        self.start_load_appx_list();
                    }

                    if ui.button(tr!("关闭")).clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_remove_appx_dialog = false;
        }
    }

    /// 启动后台加载APPX列表
    pub(crate) fn start_load_appx_list(&mut self) {
        let target = match &self.remove_appx_target {
            Some(t) => t.clone(),
            None => return,
        };

        self.remove_appx_loading = true;
        self.remove_appx_list.clear();
        self.remove_appx_selected.clear();
        self.remove_appx_message = tr!("正在加载应用列表...");

        let (tx, rx) = mpsc::channel();
        self.appx_list_rx = Some(rx);

        std::thread::spawn(move || {
            let packages = get_appx_packages(&target);
            let _ = tx.send(packages);
        });
    }

    /// 启动后台移除APPX
    fn start_remove_appx(&mut self) {
        let target = match &self.remove_appx_target {
            Some(t) => t.clone(),
            None => {
                self.remove_appx_message = tr!("请先选择目标分区");
                return;
            }
        };

        if self.remove_appx_selected.is_empty() {
            self.remove_appx_message = tr!("请先选择要移除的应用");
            return;
        }

        self.remove_appx_loading = true;
        self.remove_appx_message = tr!("正在移除应用...");

        let selected: Vec<String> = self.remove_appx_selected.iter().cloned().collect();
        let (tx, rx) = mpsc::channel();
        self.appx_remove_rx = Some(rx);

        std::thread::spawn(move || {
            let result = remove_appx_packages(&target, &selected);
            let _ = tx.send(result);
        });
    }
}
