use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::common::get_message_color;

impl App {
    // ==================== BitLocker 管理对话框 ====================

    /// 渲染 BitLocker 管理对话框
    ///
    /// 列出本机所有 BitLocker 加密分区，可对选中分区：
    /// - 已锁定：用密码 / 恢复密钥解锁；
    /// - 已解锁：彻底关闭 BitLocker（解密，后台进行）。
    pub fn render_bitlocker_manage_dialog(&mut self, ui: &mut egui::Ui) {
        use crate::app::BitLockerUnlockMode;
        use crate::core::bitlocker::VolumeStatus;

        if !self.show_bitlocker_manage_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_unlock = false;
        let mut do_decrypt = false;
        let mut do_refresh = false;
        let mut do_get_recovery = false;
        let mut do_suspend = false;
        let mut do_resume = false;
        let mut do_export_recovery = false;

        egui::Window::new(tr!("BitLocker管理"))
            .resizable(true)
            .default_width(560.0)
            .default_height(420.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr!("管理本机 BitLocker 加密分区：解锁已锁定的分区，或彻底关闭（解密）已解锁的分区。"));
                ui.add_space(10.0);

                if self.bitlocker_manage_partitions_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在检测 BitLocker 分区..."));
                    });
                } else if self.bitlocker_manage_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        tr!("未检测到 BitLocker 加密分区"),
                    );
                } else {
                    // 分区列表（单选）
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            egui::Grid::new("bitlocker_manage_partitions")
                                .num_columns(5)
                                .spacing([10.0, 4.0])
                                .min_col_width(70.0)
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(tr!("选择")).strong());
                                    ui.label(egui::RichText::new(tr!("分区")).strong());
                                    ui.label(egui::RichText::new(tr!("大小")).strong());
                                    ui.label(egui::RichText::new(tr!("卷标")).strong());
                                    ui.label(egui::RichText::new(tr!("状态")).strong());
                                    ui.end_row();

                                    for partition in &self.bitlocker_manage_partitions.clone() {
                                        let selected = self.bitlocker_manage_selected.as_ref()
                                            == Some(&partition.letter);

                                        if ui.radio(selected, "").clicked() {
                                            self.bitlocker_manage_selected = Some(partition.letter.clone());
                                            self.bitlocker_manage_message.clear();
                                            self.bitlocker_manage_password.clear();
                                            self.bitlocker_manage_recovery_key.clear();
                                            self.bitlocker_manage_recovery_display = None;
                                        }

                                        let status_color = match partition.status {
                                            VolumeStatus::EncryptedLocked => egui::Color32::from_rgb(255, 100, 100),
                                            VolumeStatus::EncryptedUnlocked => egui::Color32::from_rgb(100, 200, 100),
                                            VolumeStatus::Decrypting | VolumeStatus::Encrypting => {
                                                egui::Color32::from_rgb(100, 150, 255)
                                            }
                                            _ => egui::Color32::GRAY,
                                        };
                                        let status_text = match partition.encryption_percentage {
                                            Some(p) if matches!(
                                                partition.status,
                                                VolumeStatus::Decrypting | VolumeStatus::Encrypting
                                            ) =>
                                            {
                                                format!("{} ({}%)", tr!(partition.status.as_str()), p)
                                            }
                                            _ => tr!(partition.status.as_str()),
                                        };

                                        ui.label(&partition.letter);
                                        ui.label(format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0));
                                        ui.label(if partition.label.is_empty() { "-" } else { &partition.label });
                                        ui.colored_label(status_color, status_text);
                                        ui.end_row();
                                    }
                                });
                        });

                    ui.add_space(10.0);
                    ui.separator();

                    // 选中分区状态决定操作区
                    let selected_status = self.bitlocker_manage_selected.as_ref().and_then(|letter| {
                        self.bitlocker_manage_partitions
                            .iter()
                            .find(|p| &p.letter == letter)
                            .map(|p| p.status)
                    });

                    match selected_status {
                        Some(VolumeStatus::EncryptedLocked) => {
                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label(tr!("解锁方式:"));
                                ui.radio_value(&mut self.bitlocker_manage_mode, BitLockerUnlockMode::Password, tr!("密码"));
                                ui.radio_value(&mut self.bitlocker_manage_mode, BitLockerUnlockMode::RecoveryKey, tr!("恢复密钥"));
                            });
                            ui.add_space(5.0);
                            match self.bitlocker_manage_mode {
                                BitLockerUnlockMode::Password => {
                                    ui.horizontal(|ui| {
                                        ui.label(tr!("密码:"));
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.bitlocker_manage_password)
                                                .password(true)
                                                .desired_width(320.0),
                                        );
                                    });
                                }
                                BitLockerUnlockMode::RecoveryKey => {
                                    ui.horizontal(|ui| {
                                        ui.label(tr!("恢复密钥:"));
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.bitlocker_manage_recovery_key)
                                                .desired_width(320.0)
                                                .hint_text("000000-000000-000000-000000-000000-000000-000000-000000"),
                                        );
                                    });
                                }
                            }
                        }
                        Some(VolumeStatus::EncryptedUnlocked) => {
                            ui.add_space(5.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 200, 100),
                                tr!("该分区已解锁，可彻底关闭 BitLocker（解密）"),
                            );
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 165, 0),
                                tr!("解密在后台进行，可能耗时较长，期间请勿断电或重启。"),
                            );
                        }
                        Some(VolumeStatus::Decrypting) => {
                            ui.add_space(5.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 150, 255),
                                tr!("该分区正在解密中，请等待完成。"),
                            );
                        }
                        Some(VolumeStatus::Encrypting) => {
                            ui.add_space(5.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 150, 255),
                                tr!("该分区正在加密中。"),
                            );
                        }
                        _ => {
                            ui.add_space(5.0);
                            ui.label(tr!("请选择一个分区进行操作。"));
                        }
                    }
                }

                // 状态消息
                if !self.bitlocker_manage_message.is_empty() {
                    ui.add_space(10.0);
                    let color = get_message_color(&self.bitlocker_manage_message);
                    ui.colored_label(color, &self.bitlocker_manage_message);
                }

                // 恢复密钥展示（查看/备份）
                if let Some(key) = self.bitlocker_manage_recovery_display.clone() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        tr!("恢复密钥（48 位数字），请妥善保管、勿泄露："),
                    );
                    ui.monospace(key.as_str());
                    ui.horizontal(|ui| {
                        if ui.button(tr!("导出到文件")).clicked() {
                            do_export_recovery = true;
                        }
                        if ui.button(tr!("隐藏")).clicked() {
                            self.bitlocker_manage_recovery_display = None;
                        }
                    });
                }

                ui.add_space(15.0);
                ui.separator();
                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    if self.bitlocker_manage_loading {
                        ui.spinner();
                        ui.label(tr!("正在执行操作..."));
                    } else {
                        let selected_status = self.bitlocker_manage_selected.as_ref().and_then(|letter| {
                            self.bitlocker_manage_partitions
                                .iter()
                                .find(|p| &p.letter == letter)
                                .map(|p| p.status)
                        });

                        match selected_status {
                            Some(VolumeStatus::EncryptedLocked) => {
                                let can_unlock = match self.bitlocker_manage_mode {
                                    BitLockerUnlockMode::Password => !self.bitlocker_manage_password.is_empty(),
                                    BitLockerUnlockMode::RecoveryKey => !self.bitlocker_manage_recovery_key.is_empty(),
                                };
                                if ui.add_enabled(can_unlock, egui::Button::new(tr!("解锁"))).clicked() {
                                    do_unlock = true;
                                }
                            }
                            Some(VolumeStatus::EncryptedUnlocked) => {
                                if ui.button(tr!("关闭 BitLocker（解密）")).clicked() {
                                    do_decrypt = true;
                                }
                                if ui.button(tr!("查看恢复密钥")).clicked() {
                                    do_get_recovery = true;
                                }
                                if ui.button(tr!("挂起保护")).clicked() {
                                    do_suspend = true;
                                }
                                if ui.button(tr!("恢复保护")).clicked() {
                                    do_resume = true;
                                }
                            }
                            _ => {}
                        }

                        if ui.button(tr!("刷新")).clicked() {
                            do_refresh = true;
                        }
                        if ui.button(tr!("关闭")).clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_unlock {
            self.start_bitlocker_manage_unlock();
        }
        if do_decrypt {
            self.start_bitlocker_manage_decrypt();
        }
        if do_refresh {
            self.start_load_bitlocker_manage_partitions();
        }
        if do_get_recovery {
            self.start_bitlocker_manage_get_recovery();
        }
        if do_suspend {
            self.start_bitlocker_manage_suspend();
        }
        if do_resume {
            self.start_bitlocker_manage_resume();
        }
        if do_export_recovery {
            if let Some(key) = self.bitlocker_manage_recovery_display.clone() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("BitLocker恢复密钥.txt")
                    .save_file()
                {
                    match std::fs::write(&path, key.as_bytes()) {
                        Ok(_) => {
                            self.bitlocker_manage_message =
                                tr!("恢复密钥已导出到 {}", path.display())
                        }
                        Err(e) => self.bitlocker_manage_message = tr!("导出失败: {}", e),
                    }
                }
            }
        }
        if should_close {
            self.show_bitlocker_manage_dialog = false;
        }
    }

    /// 启动后台加载 BitLocker 加密分区列表
    pub fn start_load_bitlocker_manage_partitions(&mut self) {
        if self.bitlocker_manage_partitions_loading {
            return;
        }

        self.bitlocker_manage_partitions_loading = true;
        self.bitlocker_manage_partitions.clear();

        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_partitions_rx = Some(rx);

        std::thread::spawn(move || {
            let partitions = super::super::bitlocker::get_bitlocker_partitions();
            let _ = tx.send(partitions);
        });
    }

    /// 启动 BitLocker 解锁（管理工具）
    fn start_bitlocker_manage_unlock(&mut self) {
        use crate::app::BitLockerUnlockMode;

        if self.bitlocker_manage_loading {
            return;
        }

        let drive = match &self.bitlocker_manage_selected {
            Some(d) => d.clone(),
            None => {
                self.bitlocker_manage_message = tr!("请先选择要解锁的分区");
                return;
            }
        };

        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message = tr!("正在解锁...");

        let mode = self.bitlocker_manage_mode;
        let password = self.bitlocker_manage_password.clone();
        let recovery_key = self.bitlocker_manage_recovery_key.clone();

        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_unlock_rx = Some(rx);

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

    /// 启动 BitLocker 解密（管理工具，彻底关闭 BitLocker）
    fn start_bitlocker_manage_decrypt(&mut self) {
        if self.bitlocker_manage_loading {
            return;
        }

        let drive = match &self.bitlocker_manage_selected {
            Some(d) => d.clone(),
            None => {
                self.bitlocker_manage_message = tr!("请先选择要解密的分区");
                return;
            }
        };

        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message = tr!("正在发起解密...");

        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_decrypt_rx = Some(rx);

        std::thread::spawn(move || {
            let result = super::super::bitlocker::decrypt_partition(&drive);
            let _ = tx.send(result);
        });
    }

    /// 启动获取恢复密钥（管理工具，需分区已解锁）
    fn start_bitlocker_manage_get_recovery(&mut self) {
        if self.bitlocker_manage_loading {
            return;
        }
        let drive = match &self.bitlocker_manage_selected {
            Some(d) => d.clone(),
            None => {
                self.bitlocker_manage_message = tr!("请先选择分区");
                return;
            }
        };
        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message = tr!("正在读取恢复密钥...");
        self.bitlocker_manage_recovery_display = None;
        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_recovery_rx = Some(rx);
        std::thread::spawn(move || {
            let result = super::super::bitlocker::get_recovery_key_partition(&drive);
            let _ = tx.send(result);
        });
    }

    /// 启动挂起 BitLocker 保护
    fn start_bitlocker_manage_suspend(&mut self) {
        self.start_bitlocker_manage_protect(true);
    }

    /// 启动恢复 BitLocker 保护
    fn start_bitlocker_manage_resume(&mut self) {
        self.start_bitlocker_manage_protect(false);
    }

    fn start_bitlocker_manage_protect(&mut self, suspend: bool) {
        if self.bitlocker_manage_loading {
            return;
        }
        let drive = match &self.bitlocker_manage_selected {
            Some(d) => d.clone(),
            None => {
                self.bitlocker_manage_message = tr!("请先选择分区");
                return;
            }
        };
        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message =
            if suspend { tr!("正在挂起保护...") } else { tr!("正在恢复保护...") };
        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_protect_rx = Some(rx);
        std::thread::spawn(move || {
            let result = if suspend {
                super::super::bitlocker::suspend_partition_protection(&drive)
            } else {
                super::super::bitlocker::resume_partition_protection(&drive)
            };
            let _ = tx.send(result);
        });
    }

    /// 检查 BitLocker 管理工具的异步操作结果
    pub(crate) fn check_bitlocker_manage_async_operations(&mut self) {
        // 分区列表加载结果
        if let Some(ref rx) = self.bitlocker_manage_partitions_rx {
            if let Ok(partitions) = rx.try_recv() {
                // 若先前选中的分区仍在列表中则保留选择，否则默认选中第一个
                let keep = self
                    .bitlocker_manage_selected
                    .as_ref()
                    .map(|sel| partitions.iter().any(|p| &p.letter == sel))
                    .unwrap_or(false);
                if !keep {
                    self.bitlocker_manage_selected = partitions.first().map(|p| p.letter.clone());
                }
                self.bitlocker_manage_partitions = partitions;
                self.bitlocker_manage_partitions_loading = false;
                self.bitlocker_manage_partitions_rx = None;
            }
        }

        // 解锁结果
        if let Some(ref rx) = self.bitlocker_manage_unlock_rx {
            if let Ok(result) = rx.try_recv() {
                self.bitlocker_manage_loading = false;
                self.bitlocker_manage_unlock_rx = None;
                if result.success {
                    self.bitlocker_manage_message = tr!("{} 解锁成功", result.letter);
                    self.bitlocker_manage_password.clear();
                    self.bitlocker_manage_recovery_key.clear();
                    // 刷新列表以更新状态
                    self.start_load_bitlocker_manage_partitions();
                } else {
                    self.bitlocker_manage_message =
                        tr!("{} 解锁失败: {}", result.letter, result.message);
                }
            }
        }

        // 解密结果
        if let Some(ref rx) = self.bitlocker_manage_decrypt_rx {
            if let Ok(result) = rx.try_recv() {
                self.bitlocker_manage_loading = false;
                self.bitlocker_manage_decrypt_rx = None;
                if result.success {
                    self.bitlocker_manage_message = format!("{}: {}", result.letter, result.message);
                    // 刷新列表以更新状态
                    self.start_load_bitlocker_manage_partitions();
                } else {
                    self.bitlocker_manage_message =
                        tr!("{} 解密失败: {}", result.letter, result.message);
                }
            }
        }

        // 恢复密钥读取结果
        if let Some(ref rx) = self.bitlocker_manage_recovery_rx {
            if let Ok(result) = rx.try_recv() {
                self.bitlocker_manage_loading = false;
                self.bitlocker_manage_recovery_rx = None;
                match result {
                    Ok(key) => {
                        self.bitlocker_manage_recovery_display = Some(key);
                        self.bitlocker_manage_message = tr!("已读取恢复密钥");
                    }
                    Err(e) => {
                        self.bitlocker_manage_message = tr!("读取恢复密钥失败: {}", e);
                    }
                }
            }
        }

        // 挂起/恢复保护结果
        if let Some(ref rx) = self.bitlocker_manage_protect_rx {
            if let Ok(result) = rx.try_recv() {
                self.bitlocker_manage_loading = false;
                self.bitlocker_manage_protect_rx = None;
                match result {
                    Ok(msg) => {
                        self.bitlocker_manage_message = msg;
                        self.start_load_bitlocker_manage_partitions();
                    }
                    Err(e) => {
                        self.bitlocker_manage_message = tr!("操作失败: {}", e);
                    }
                }
            }
        }
    }
}
