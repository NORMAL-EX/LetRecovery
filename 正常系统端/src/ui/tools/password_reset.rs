//! 离线密码重置对话框
//!
//! 对**离线的** Windows 安装（如另一块盘/分区上的系统、整盘备份还原后的系统）：
//! 清除指定本地账户的密码（等效空密码）并启用被禁用的账户。
//! 复用共享库 `lr_core::sam`：先选目标系统、列出其本地账户，再点选某账户清除密码。
//! （`clear_account_password` 含强制备份、成功后删除备份等安全措施。）
//!
//! 需要管理员权限；目标分区须存在 `\Windows\System32\config\SAM`。

use egui;
use std::sync::mpsc;

use crate::app::App;

impl App {
    /// 渲染离线密码重置对话框
    pub fn render_password_reset_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_password_reset_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_reset = false;

        // 与其它工具一致：用检测到的 Windows 分区作为“目标系统”候选。
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;
        let old_target = self.password_reset_target.clone();

        egui::Window::new("🔑 离线密码重置")
            .resizable(true)
            .default_width(560.0)
            .default_height(360.0)
            .show(ui.ctx(), |ui| {
                ui.label("清除离线 Windows 本地账户的密码（等效空密码），并启用被禁用的账户。");
                ui.colored_label(
                    egui::Color32::from_rgb(255, 165, 0),
                    "⚠ 仅用于自己的系统/已授权场景；将修改目标系统的 SAM（操作前自动备份）。",
                );
                ui.add_space(10.0);

                // 目标系统选择（下拉框）
                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测 Windows 分区...");
                    });
                } else if windows_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "未检测到可用的离线 Windows 系统。",
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("目标系统:");

                        let current_text = self
                            .password_reset_target
                            .as_ref()
                            .map(|letter| {
                                windows_partitions
                                    .iter()
                                    .find(|p| &p.letter == letter)
                                    .map(|p| {
                                        format!(
                                            "{} [{}] [{}]",
                                            p.letter, p.windows_version, p.architecture
                                        )
                                    })
                                    .unwrap_or_else(|| letter.clone())
                            })
                            .unwrap_or_else(|| "请选择".to_string());

                        egui::ComboBox::from_id_salt("password_reset_partition")
                            .selected_text(current_text)
                            .show_ui(ui, |ui| {
                                for partition in &windows_partitions {
                                    let display = format!(
                                        "{} [{}] [{}]",
                                        partition.letter,
                                        partition.windows_version,
                                        partition.architecture
                                    );
                                    ui.selectable_value(
                                        &mut self.password_reset_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });

                        if ui.button("🔄 刷新").clicked() {
                            self.refresh_windows_partitions_cache();
                        }
                    });
                }

                ui.add_space(8.0);

                // 账户列表
                if self.password_reset_users_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在读取账户列表...");
                    });
                } else if self.password_reset_target.is_some()
                    && !self.password_reset_users.is_empty()
                {
                    ui.label("选择要重置密码的账户:");
                    ui.add_space(2.0);
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            // 先克隆一份用于展示，避免在迭代时借用冲突
                            let users = self.password_reset_users.clone();
                            for acc in &users {
                                let selected = self.password_reset_selected_user.as_deref()
                                    == Some(acc.username.as_str());
                                let label = if acc.disabled {
                                    format!("{}（已禁用）", acc.username)
                                } else {
                                    acc.username.clone()
                                };
                                if ui.selectable_label(selected, label).clicked() {
                                    self.password_reset_selected_user = Some(acc.username.clone());
                                }
                            }
                        });
                } else if self.password_reset_target.is_some() {
                    ui.colored_label(egui::Color32::GRAY, "该系统中未找到本地账户。");
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let can_reset = !self.password_reset_loading
                        && self.password_reset_target.is_some()
                        && self.password_reset_selected_user.is_some();
                    if ui
                        .add_enabled(can_reset, egui::Button::new("重置所选账户密码"))
                        .clicked()
                    {
                        do_reset = true;
                    }
                    if self.password_reset_loading {
                        ui.add_space(10.0);
                        ui.spinner();
                        ui.label("正在处理...");
                    }
                });

                if !self.password_reset_message.is_empty() {
                    ui.add_space(10.0);
                    ui.separator();
                    let color = if self.password_reset_message.starts_with('✓') {
                        egui::Color32::from_rgb(0, 200, 0)
                    } else if self.password_reset_message.starts_with('✗') {
                        egui::Color32::from_rgb(255, 80, 80)
                    } else {
                        egui::Color32::GRAY
                    };
                    ui.colored_label(color, &self.password_reset_message);
                }

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        // 目标系统改变：清空旧状态并加载该系统的账户列表
        if self.password_reset_target != old_target && self.password_reset_target.is_some() {
            self.start_load_password_reset_users();
        }

        if do_reset {
            self.start_password_reset();
        }
        if should_close {
            self.show_password_reset_dialog = false;
        }
    }

    /// 启动加载目标系统的本地账户列表（后台线程，只读）
    fn start_load_password_reset_users(&mut self) {
        let partition = match self.normalized_target_partition() {
            Some(p) => p,
            None => return,
        };

        self.password_reset_users_loading = true;
        self.password_reset_users.clear();
        self.password_reset_selected_user = None;
        self.password_reset_message.clear();

        let (tx, rx) = mpsc::channel::<Result<Vec<lr_core::sam::SamAccount>, String>>();
        self.password_reset_users_rx = Some(rx);

        std::thread::spawn(move || {
            let result = lr_core::sam::list_accounts(&partition).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// 轮询账户列表加载状态（在主循环中调用）
    pub fn check_password_reset_users_status(&mut self) {
        if let Some(ref rx) = self.password_reset_users_rx {
            if let Ok(result) = rx.try_recv() {
                self.password_reset_users_loading = false;
                self.password_reset_users_rx = None;
                match result {
                    Ok(users) => {
                        self.password_reset_users = users;
                    }
                    Err(e) => {
                        self.password_reset_users.clear();
                        self.password_reset_message = format!("✗ 读取账户列表失败：{}", e);
                    }
                }
            }
        }
    }

    /// 把选中的目标系统盘符规范化为 "X:"。
    fn normalized_target_partition(&self) -> Option<String> {
        let raw = self.password_reset_target.as_ref()?.trim();
        let letter = raw.chars().next()?;
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        Some(format!("{}:", letter.to_ascii_uppercase()))
    }

    /// 启动离线密码重置（后台线程）
    fn start_password_reset(&mut self) {
        if self.password_reset_loading {
            return;
        }
        let partition = match self.normalized_target_partition() {
            Some(p) => p,
            None => {
                self.password_reset_message = "✗ 请先选择目标系统".to_string();
                return;
            }
        };
        let username = match self.password_reset_selected_user.clone() {
            Some(u) if !u.trim().is_empty() => u.trim().to_string(),
            _ => {
                self.password_reset_message = "✗ 请先在列表中选择一个账户".to_string();
                return;
            }
        };

        // 预检查 SAM 是否存在，给出更友好的提示
        let sam = format!("{}\\Windows\\System32\\config\\SAM", partition);
        if !std::path::Path::new(&sam).exists() {
            self.password_reset_message =
                format!("✗ 未在 {} 找到 Windows（缺少 {}）", partition, sam);
            return;
        }

        self.password_reset_loading = true;
        self.password_reset_partition = partition.clone();
        self.password_reset_username = username.clone();
        self.password_reset_message = format!("正在重置账户 [{}] 的密码...", username);

        let (tx, rx) = mpsc::channel::<Result<bool, String>>();
        self.password_reset_rx = Some(rx);

        std::thread::spawn(move || {
            let result = lr_core::sam::clear_account_password(&partition, &username)
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// 轮询离线密码重置状态（在主循环中调用）
    pub fn check_password_reset_status(&mut self) {
        if let Some(ref rx) = self.password_reset_rx {
            if let Ok(result) = rx.try_recv() {
                self.password_reset_loading = false;
                self.password_reset_rx = None;
                let reload = matches!(result, Ok(true));
                self.password_reset_message = match result {
                    Ok(true) => {
                        "✓ 已重置该账户密码（可空密码登录），并已启用账户".to_string()
                    }
                    Ok(false) => {
                        "✗ 未找到匹配的账户（请核对用户名），SAM 未改动".to_string()
                    }
                    Err(e) => format!("✗ 失败：{}", e),
                };
                // 成功后刷新账户列表（更新“已禁用”标记），但保留成功提示
                if reload {
                    let msg = self.password_reset_message.clone();
                    self.start_load_password_reset_users();
                    self.password_reset_message = msg;
                }
            }
        }
    }
}
