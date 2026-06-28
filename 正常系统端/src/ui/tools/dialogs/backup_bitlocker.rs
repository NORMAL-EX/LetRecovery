use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::common::get_message_color;

impl App {
    // ==================== 备份时BitLocker解锁对话框 ====================

    /// 渲染备份时BitLocker解锁对话框
    pub fn render_backup_bitlocker_dialog(&mut self, ui: &mut egui::Ui) {
        use crate::app::BitLockerUnlockMode;
        use crate::core::bitlocker::VolumeStatus;

        if !self.show_backup_bitlocker_dialog {
            return;
        }

        // 检查解锁结果
        self.check_backup_bitlocker_unlock_result();

        let mut should_close = false;
        let mut do_unlock = false;
        let mut do_skip = false;
        let mut do_skip_all = false;

        egui::Window::new(tr!("BitLocker解锁 - 备份"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.set_min_width(500.0);

                ui.label(tr!("检测到以下分区被BitLocker加密锁定，需要解锁后才能继续备份："));
                ui.add_space(10.0);

                // 显示锁定分区列表
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .show(ui, |ui| {
                        egui::Grid::new("backup_bitlocker_partitions")
                            .num_columns(4)
                            .spacing([10.0, 4.0])
                            .min_col_width(80.0)
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(tr!("分区")).strong());
                                ui.label(egui::RichText::new(tr!("大小")).strong());
                                ui.label(egui::RichText::new(tr!("卷标")).strong());
                                ui.label(egui::RichText::new(tr!("状态")).strong());
                                ui.end_row();

                                for partition in &self.backup_bitlocker_partitions {
                                    let is_current = self.backup_bitlocker_current.as_ref() == Some(&partition.letter);
                                    
                                    let status_color = match partition.status {
                                        VolumeStatus::EncryptedLocked => egui::Color32::from_rgb(255, 100, 100),
                                        VolumeStatus::EncryptedUnlocked => egui::Color32::from_rgb(100, 200, 100),
                                        _ => egui::Color32::GRAY,
                                    };
                                    
                                    let label = if is_current {
                                        egui::RichText::new(&partition.letter).strong().color(egui::Color32::from_rgb(100, 150, 255))
                                    } else {
                                        egui::RichText::new(&partition.letter)
                                    };
                                    
                                    ui.label(label);
                                    ui.label(format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0));
                                    ui.label(if partition.label.is_empty() { "-" } else { &partition.label });
                                    ui.colored_label(status_color, tr!(partition.status.as_str()));
                                    ui.end_row();
                                }
                            });
                    });

                ui.add_space(10.0);
                ui.separator();

                // 检查是否还有需要解锁的分区
                let has_locked = self.backup_bitlocker_partitions.iter()
                    .any(|p| p.status == VolumeStatus::EncryptedLocked);

                if has_locked {
                    // 显示当前要解锁的分区
                    if let Some(ref current) = self.backup_bitlocker_current {
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            ui.label(tr!("当前解锁:"));
                            ui.strong(current);
                        });
                    }

                    ui.add_space(10.0);

                    // 解锁模式选择
                    ui.horizontal(|ui| {
                        ui.label(tr!("解锁方式:"));
                        ui.radio_value(&mut self.backup_bitlocker_mode, BitLockerUnlockMode::Password, tr!("密码"));
                        ui.radio_value(&mut self.backup_bitlocker_mode, BitLockerUnlockMode::RecoveryKey, tr!("恢复密钥"));
                    });

                    ui.add_space(5.0);

                    // 输入框
                    match self.backup_bitlocker_mode {
                        BitLockerUnlockMode::Password => {
                            ui.horizontal(|ui| {
                                ui.label(tr!("密码:"));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.backup_bitlocker_password)
                                        .password(true)
                                        .desired_width(300.0),
                                );
                            });
                        }
                        BitLockerUnlockMode::RecoveryKey => {
                            ui.horizontal(|ui| {
                                ui.label(tr!("恢复密钥:"));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.backup_bitlocker_recovery_key)
                                        .desired_width(300.0)
                                        .hint_text("000000-000000-000000-000000-000000-000000-000000-000000"),
                                );
                            });
                        }
                    }
                } else {
                    // 所有分区都已解锁
                    ui.add_space(10.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(100, 200, 100),
                        tr!("所有分区已解锁，可以继续备份"),
                    );
                }

                // 显示消息
                if !self.backup_bitlocker_message.is_empty() {
                    ui.add_space(10.0);
                    let color = get_message_color(&self.backup_bitlocker_message);
                    ui.colored_label(color, &self.backup_bitlocker_message);
                }

                ui.add_space(15.0);
                ui.separator();
                ui.add_space(5.0);

                // 按钮
                ui.horizontal(|ui| {
                    if self.backup_bitlocker_loading {
                        ui.spinner();
                        ui.label(tr!("正在解锁..."));
                    } else if has_locked {
                        let can_unlock = self.backup_bitlocker_current.is_some()
                            && match self.backup_bitlocker_mode {
                                BitLockerUnlockMode::Password => !self.backup_bitlocker_password.is_empty(),
                                BitLockerUnlockMode::RecoveryKey => !self.backup_bitlocker_recovery_key.is_empty(),
                            };

                        if ui.add_enabled(can_unlock, egui::Button::new(tr!("解锁"))).clicked() {
                            do_unlock = true;
                        }

                        if ui.button(tr!("跳过此分区")).clicked() {
                            do_skip = true;
                        }

                        if ui.button(tr!("跳过所有")).clicked() {
                            do_skip_all = true;
                        }

                        if ui.button(tr!("取消备份")).clicked() {
                            should_close = true;
                        }
                    } else {
                        // 所有分区都已解锁
                        if ui.button(tr!("继续备份")).clicked() {
                            should_close = true;
                            if self.backup_bitlocker_continue_after {
                                self.continue_backup_after_bitlocker();
                            }
                        }

                        if ui.button(tr!("取消")).clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        // 处理操作
        if do_unlock {
            self.start_backup_bitlocker_unlock();
        }

        if do_skip {
            self.skip_current_backup_bitlocker_partition();
        }

        if do_skip_all {
            // 跳过所有锁定的分区
            self.backup_bitlocker_partitions.retain(|p| p.status != VolumeStatus::EncryptedLocked);
            self.backup_bitlocker_current = None;
            self.backup_bitlocker_message = tr!("已跳过所有锁定的分区");
        }

        if should_close {
            self.show_backup_bitlocker_dialog = false;
            self.backup_bitlocker_continue_after = false;
        }
    }

    /// 检查备份时BitLocker解锁结果
    fn check_backup_bitlocker_unlock_result(&mut self) {
        use crate::core::bitlocker::VolumeStatus;

        if let Some(ref rx) = self.backup_bitlocker_rx {
            if let Ok(result) = rx.try_recv() {
                self.backup_bitlocker_loading = false;
                self.backup_bitlocker_rx = None;

                if result.success {
                    self.backup_bitlocker_message = tr!("{} 解锁成功", result.letter);

                    // 更新分区状态
                    if let Some(partition) = self.backup_bitlocker_partitions.iter_mut()
                        .find(|p| p.letter == result.letter)
                    {
                        partition.status = VolumeStatus::EncryptedUnlocked;
                    }

                    // 清空输入
                    self.backup_bitlocker_password.clear();
                    self.backup_bitlocker_recovery_key.clear();

                    // 选择下一个需要解锁的分区
                    self.select_next_backup_bitlocker_partition();
                } else {
                    self.backup_bitlocker_message = tr!("{} 解锁失败: {}", result.letter, result.message);
                }
            }
        }
    }

    /// 启动备份时BitLocker解锁
    fn start_backup_bitlocker_unlock(&mut self) {
        use crate::app::BitLockerUnlockMode;

        if self.backup_bitlocker_loading {
            return;
        }

        let drive = match &self.backup_bitlocker_current {
            Some(d) => d.clone(),
            None => {
                self.backup_bitlocker_message = tr!("请先选择要解锁的分区");
                return;
            }
        };

        self.backup_bitlocker_loading = true;
        self.backup_bitlocker_message = tr!("正在解锁...");

        let mode = self.backup_bitlocker_mode;
        let password = self.backup_bitlocker_password.clone();
        let recovery_key = self.backup_bitlocker_recovery_key.clone();

        let (tx, rx) = mpsc::channel();
        self.backup_bitlocker_rx = Some(rx);

        std::thread::spawn(move || {
            let result = match mode {
                BitLockerUnlockMode::Password => {
                    super::super::bitlocker::unlock_with_password(&drive, &password)
                }
                BitLockerUnlockMode::RecoveryKey => {
                    super::super::bitlocker::unlock_with_recovery_key(&drive, &recovery_key)
                }
            };
            let _ = tx.send(result);
        });
    }

    /// 跳过当前备份时BitLocker分区
    fn skip_current_backup_bitlocker_partition(&mut self) {
        if let Some(ref current) = self.backup_bitlocker_current.clone() {
            // 从列表中移除当前分区
            self.backup_bitlocker_partitions.retain(|p| p.letter != *current);
            self.backup_bitlocker_message = tr!("已跳过分区 {}", current);
            
            // 选择下一个需要解锁的分区
            self.select_next_backup_bitlocker_partition();
        }
    }

    /// 选择下一个需要解锁的备份时BitLocker分区
    fn select_next_backup_bitlocker_partition(&mut self) {
        use crate::core::bitlocker::VolumeStatus;

        self.backup_bitlocker_current = self.backup_bitlocker_partitions
            .iter()
            .find(|p| p.status == VolumeStatus::EncryptedLocked)
            .map(|p| p.letter.clone());
    }
}
