//! 对话框渲染模块
//!
//! 提供工具箱各种对话框的渲染功能

use egui;
use std::collections::HashSet;
use std::sync::mpsc;
use crate::app::App;
use super::types::{DriverBackupMode, WindowsPartitionInfo};
use super::version_detect::get_windows_partition_infos;
use super::network::get_detailed_network_info;
use super::appx::{get_appx_packages, remove_appx_packages};
use super::software::{truncate_string, save_software_list_to_file, get_installed_software};
use super::network::reset_network;

impl App {
    /// 检查并处理异步操作结果
    pub fn check_tools_async_operations(&mut self) {
        // 检查Windows分区信息加载结果
        if let Some(ref rx) = self.windows_partitions_rx {
            if let Ok(partitions) = rx.try_recv() {
                self.windows_partitions_cache = Some(partitions);
                self.windows_partitions_loading = false;
                self.windows_partitions_rx = None;
            }
        }
        
        // 检查驱动操作结果
        if let Some(ref rx) = self.driver_operation_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(msg) => {
                        self.driver_backup_message = msg;
                    }
                    Err(msg) => {
                        self.driver_backup_message = msg;
                    }
                }
                self.driver_backup_loading = false;
                self.driver_operation_rx = None;
            }
        }
        
        // 检查存储驱动导入结果
        if let Some(ref rx) = self.storage_driver_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(msg) => {
                        self.import_storage_driver_message = msg;
                    }
                    Err(msg) => {
                        self.import_storage_driver_message = msg;
                    }
                }
                self.import_storage_driver_loading = false;
                self.storage_driver_rx = None;
            }
        }
        
        // 检查APPX列表加载结果
        if let Some(ref rx) = self.appx_list_rx {
            if let Ok(packages) = rx.try_recv() {
                if packages.is_empty() {
                    self.remove_appx_message = "未找到可移除的应用".to_string();
                } else {
                    self.remove_appx_message.clear();
                }
                self.remove_appx_list = packages;
                self.remove_appx_loading = false;
                self.appx_list_rx = None;
            }
        }
        
        // 检查APPX移除结果
        if let Some(ref rx) = self.appx_remove_rx {
            if let Ok((success, fail)) = rx.try_recv() {
                self.remove_appx_message = format!("移除完成: 成功 {}, 失败 {}", success, fail);
                self.remove_appx_loading = false;
                self.appx_remove_rx = None;
                // 刷新列表
                self.start_load_appx_list();
            }
        }
        
        // 检查时间同步结果
        if let Some(ref rx) = self.time_sync_rx {
            if let Ok(result) = rx.try_recv() {
                if result.success {
                    self.time_sync_message = format!(
                        "{}\n\n原时间: {}\n新时间: {}",
                        result.message,
                        result.old_time.unwrap_or_default(),
                        result.new_time.unwrap_or_default()
                    );
                } else {
                    self.time_sync_message = result.message;
                }
                self.time_sync_loading = false;
                self.time_sync_rx = None;
            }
        }
        
        // 检查批量格式化分区列表加载结果
        if let Some(ref rx) = self.batch_format_partitions_rx {
            if let Ok(partitions) = rx.try_recv() {
                self.batch_format_partitions = partitions;
                self.batch_format_partitions_loading = false;
                self.batch_format_partitions_rx = None;
            }
        }
        
        // 检查批量格式化结果
        if let Some(ref rx) = self.batch_format_rx {
            if let Ok(result) = rx.try_recv() {
                let mut msg = format!(
                    "格式化完成: 成功 {}, 失败 {}",
                    result.success_count, result.fail_count
                );
                for r in &result.results {
                    msg.push_str(&format!("\n{}: {}", r.letter, r.message));
                }
                self.batch_format_message = msg;
                self.batch_format_loading = false;
                self.batch_format_rx = None;
                // 刷新分区列表
                self.start_load_formatable_partitions();
            }
        }
        
        // 检查GHO密码读取结果
        self.check_gho_password_result();
        
        // 检查英伟达驱动卸载结果
        self.check_nvidia_uninstall_result();
        
        // 检查分区对拷异步操作
        self.check_partition_copy_async_operations();
        
        // 检查一键分区异步操作
        self.check_quick_partition_disk_load();
        
        // 检查镜像校验状态
        self.check_image_verify_status();

        // 检查 BitLocker 管理工具异步操作
        self.check_bitlocker_manage_async_operations();

        // 检查文件哈希校验状态
        self.check_hash_verify_status();

        // 检查离线密码重置状态
        self.check_password_reset_status();
        self.check_password_reset_users_status();
    }
    
    /// 启动后台加载Windows分区信息
    pub fn start_load_windows_partitions(&mut self) {
        if self.windows_partitions_loading {
            return;
        }
        
        self.windows_partitions_loading = true;
        let partitions = self.partitions.clone();
        
        let (tx, rx) = mpsc::channel();
        self.windows_partitions_rx = Some(rx);
        
        std::thread::spawn(move || {
            let result = get_windows_partition_infos(&partitions);
            let _ = tx.send(result);
        });
    }
    
    /// 获取缓存的Windows分区信息，如果没有则启动加载
    pub fn get_cached_windows_partitions(&mut self) -> Vec<WindowsPartitionInfo> {
        if self.windows_partitions_cache.is_none() && !self.windows_partitions_loading {
            self.start_load_windows_partitions();
        }
        self.windows_partitions_cache.clone().unwrap_or_default()
    }
    
    /// 刷新Windows分区缓存
    pub fn refresh_windows_partitions_cache(&mut self) {
        self.windows_partitions_cache = None;
        self.start_load_windows_partitions();
    }

    /// 渲染网络信息对话框
    pub fn render_network_info_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_network_info_dialog {
            return;
        }

        egui::Window::new("本机网络信息")
            .open(&mut self.show_network_info_dialog)
            .resizable(true)
            .default_width(500.0)
            .default_height(400.0)
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(ref adapters) = self.network_info_cache {
                        if adapters.is_empty() {
                            ui.label("未检测到网络适配器");
                        } else {
                            for (i, adapter) in adapters.iter().enumerate() {
                                egui::CollapsingHeader::new(format!(
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
                                            ui.label("名称:");
                                            ui.label(&adapter.name);
                                            ui.end_row();

                                            ui.label("描述:");
                                            ui.label(&adapter.description);
                                            ui.end_row();

                                            if !adapter.adapter_type.is_empty() {
                                                ui.label("类型:");
                                                ui.label(&adapter.adapter_type);
                                                ui.end_row();
                                            }

                                            if !adapter.mac_address.is_empty() {
                                                ui.label("MAC 地址:");
                                                ui.label(&adapter.mac_address);
                                                ui.end_row();
                                            }

                                            if !adapter.ip_addresses.is_empty() {
                                                ui.label("IP 地址:");
                                                for ip in &adapter.ip_addresses {
                                                    ui.label(ip);
                                                    ui.end_row();
                                                    ui.label("");
                                                }
                                            }

                                            if !adapter.status.is_empty() {
                                                ui.label("状态:");
                                                ui.label(&adapter.status);
                                                ui.end_row();
                                            }

                                            if adapter.speed > 0 {
                                                ui.label("速度:");
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
                        ui.label("正在获取网络信息...");
                    }
                });
            });
    }

    /// 渲染导入存储驱动对话框
    pub fn render_import_storage_driver_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_import_storage_driver_dialog {
            return;
        }

        let mut should_close = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;

        egui::Window::new("导入硬盘控制器驱动")
            .resizable(false)
            .default_width(450.0)
            .show(ui.ctx(), |ui| {
                ui.label("将 Intel VMD / Apple SSD / Visior 等硬盘控制器驱动导入到离线系统");
                ui.add_space(10.0);

                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测Windows分区...");
                    });
                } else if windows_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "未找到包含 Windows 系统的分区",
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("目标分区:");
                        
                        let current_text = self
                            .import_storage_driver_target
                            .as_ref()
                            .map(|letter| {
                                format_partition_display(&windows_partitions, letter)
                            })
                            .unwrap_or_else(|| "请选择".to_string());

                        egui::ComboBox::from_id_salt("import_storage_driver_partition")
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
                                        &mut self.import_storage_driver_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });
                }

                ui.add_space(15.0);

                // 状态消息
                if !self.import_storage_driver_message.is_empty() {
                    let color = get_message_color(&self.import_storage_driver_message);
                    ui.colored_label(color, &self.import_storage_driver_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    let can_import = self.import_storage_driver_target.is_some()
                        && !self.import_storage_driver_loading
                        && !is_loading_partitions;

                    if self.import_storage_driver_loading {
                        ui.spinner();
                        ui.label("正在导入驱动...");
                    } else {
                        if ui.add_enabled(can_import, egui::Button::new("导入驱动")).clicked() {
                            self.start_import_storage_driver();
                        }
                    }

                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_import_storage_driver_dialog = false;
        }
    }

    /// 启动后台导入存储驱动
    fn start_import_storage_driver(&mut self) {
        let target = match &self.import_storage_driver_target {
            Some(t) => t.clone(),
            None => {
                self.import_storage_driver_message = "请先选择目标分区".to_string();
                return;
            }
        };

        // 检查驱动目录是否存在
        let driver_dir = crate::utils::path::get_drivers_dir()
            .join("storage_controller");

        if !driver_dir.exists() {
            self.import_storage_driver_message =
                format!("驱动目录不存在: {}", driver_dir.display());
            return;
        }

        self.import_storage_driver_loading = true;
        self.import_storage_driver_message = "正在导入驱动...".to_string();

        let driver_dir_str = driver_dir.to_string_lossy().to_string();
        let (tx, rx) = mpsc::channel();
        self.storage_driver_rx = Some(rx);

        std::thread::spawn(move || {
            let dism = crate::core::dism::Dism::new();
            let result = match dism.add_drivers_offline(&target, &driver_dir_str) {
                Ok(_) => Ok("驱动导入成功！".to_string()),
                Err(e) => Err(format!("驱动导入失败: {}", e)),
            };
            let _ = tx.send(result);
        });
    }

    /// 渲染移除APPX对话框
    pub fn render_remove_appx_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_remove_appx_dialog {
            return;
        }

        let mut should_close = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;
        let is_pe = self.is_pe_environment();

        egui::Window::new("移除APPX应用")
            .resizable(true)
            .default_width(550.0)
            .default_height(450.0)
            .show(ui.ctx(), |ui| {
                if is_pe {
                    ui.label("移除离线系统中预装的 Microsoft Store 应用");
                } else {
                    ui.label("移除当前系统或离线系统中的 Microsoft Store 应用");
                }
                ui.add_space(10.0);

                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测Windows分区...");
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label("目标系统:");

                        let current_text = self
                            .remove_appx_target
                            .as_ref()
                            .map(|letter| {
                                if letter == "__CURRENT__" {
                                    "当前系统".to_string()
                                } else {
                                    format_partition_display(&windows_partitions, letter)
                                }
                            })
                            .unwrap_or_else(|| "请选择".to_string());

                        let old_target = self.remove_appx_target.clone();

                        egui::ComboBox::from_id_salt("remove_appx_partition")
                            .selected_text(current_text)
                            .show_ui(ui, |ui| {
                                // 非PE环境显示"当前系统"选项
                                if !is_pe {
                                    ui.selectable_value(
                                        &mut self.remove_appx_target,
                                        Some("__CURRENT__".to_string()),
                                        "当前系统",
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
                        ui.label("正在处理...");
                    });
                } else if !self.remove_appx_list.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.button("全选").clicked() {
                            for pkg in &self.remove_appx_list {
                                self.remove_appx_selected
                                    .insert(pkg.package_name.clone());
                            }
                        }
                        if ui.button("反选").clicked() {
                            let current: HashSet<_> = self.remove_appx_selected.clone();
                            self.remove_appx_selected.clear();
                            for pkg in &self.remove_appx_list {
                                if !current.contains(&pkg.package_name) {
                                    self.remove_appx_selected
                                        .insert(pkg.package_name.clone());
                                }
                            }
                        }
                        ui.label(format!("已选择 {} 个应用", self.remove_appx_selected.len()));
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
                    ui.label("未找到可移除的应用，或请先点击刷新列表按钮");
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
                        .add_enabled(can_remove, egui::Button::new("移除选中应用"))
                        .clicked()
                    {
                        self.start_remove_appx();
                    }

                    let can_refresh = self.remove_appx_target.is_some() 
                        && !self.remove_appx_loading
                        && !is_loading_partitions;
                    if ui.add_enabled(can_refresh, egui::Button::new("刷新列表")).clicked() {
                        self.start_load_appx_list();
                    }

                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_remove_appx_dialog = false;
        }
    }

    /// 启动后台加载APPX列表
    fn start_load_appx_list(&mut self) {
        let target = match &self.remove_appx_target {
            Some(t) => t.clone(),
            None => return,
        };

        self.remove_appx_loading = true;
        self.remove_appx_list.clear();
        self.remove_appx_selected.clear();
        self.remove_appx_message = "正在加载应用列表...".to_string();

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
                self.remove_appx_message = "请先选择目标分区".to_string();
                return;
            }
        };

        if self.remove_appx_selected.is_empty() {
            self.remove_appx_message = "请先选择要移除的应用".to_string();
            return;
        }

        self.remove_appx_loading = true;
        self.remove_appx_message = "正在移除应用...".to_string();

        let selected: Vec<String> = self.remove_appx_selected.iter().cloned().collect();
        let (tx, rx) = mpsc::channel();
        self.appx_remove_rx = Some(rx);

        std::thread::spawn(move || {
            let result = remove_appx_packages(&target, &selected);
            let _ = tx.send(result);
        });
    }

    /// 渲染驱动备份还原对话框
    pub fn render_driver_backup_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_driver_backup_dialog {
            return;
        }

        let mut should_close = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;

        egui::Window::new("驱动备份还原")
            .resizable(false)
            .default_width(500.0)
            .show(ui.ctx(), |ui| {
                ui.label("导出或导入系统驱动");
                ui.add_space(10.0);

                // 模式选择
                ui.horizontal(|ui| {
                    ui.label("操作模式:");
                    ui.radio_value(&mut self.driver_backup_mode, DriverBackupMode::Export, "导出驱动");
                    ui.radio_value(&mut self.driver_backup_mode, DriverBackupMode::Import, "导入驱动");
                });

                ui.add_space(10.0);

                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测Windows分区...");
                    });
                } else {
                    // 根据模式显示不同选项
                    match self.driver_backup_mode {
                        DriverBackupMode::Export => {
                            ui.horizontal(|ui| {
                                ui.label("源系统分区:");
                                
                                let current_text = self
                                    .driver_backup_target
                                    .as_ref()
                                    .map(|letter| format_partition_display(&windows_partitions, letter))
                                    .unwrap_or_else(|| "请选择".to_string());

                                egui::ComboBox::from_id_salt("driver_backup_source")
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
                                                &mut self.driver_backup_target,
                                                Some(partition.letter.clone()),
                                                display,
                                            );
                                        }
                                    });
                            });

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label("保存目录:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.driver_backup_path)
                                        .desired_width(300.0),
                                );
                                if ui.button("浏览...").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        self.driver_backup_path = path.to_string_lossy().to_string();
                                    }
                                }
                            });
                        }
                        DriverBackupMode::Import => {
                            ui.horizontal(|ui| {
                                ui.label("目标系统分区:");
                                
                                let current_text = self
                                    .driver_backup_target
                                    .as_ref()
                                    .map(|letter| format_partition_display(&windows_partitions, letter))
                                    .unwrap_or_else(|| "请选择".to_string());

                                egui::ComboBox::from_id_salt("driver_import_target")
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
                                                &mut self.driver_backup_target,
                                                Some(partition.letter.clone()),
                                                display,
                                            );
                                        }
                                    });
                            });

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label("驱动目录:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.driver_backup_path)
                                        .desired_width(300.0),
                                );
                                if ui.button("浏览...").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        self.driver_backup_path = path.to_string_lossy().to_string();
                                    }
                                }
                            });
                        }
                    }
                }

                ui.add_space(15.0);

                // 状态消息
                if !self.driver_backup_message.is_empty() {
                    let color = get_message_color(&self.driver_backup_message);
                    ui.colored_label(color, &self.driver_backup_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.driver_backup_loading {
                        ui.spinner();
                        ui.label("正在处理，请稍候...");
                    } else {
                        let button_label = match self.driver_backup_mode {
                            DriverBackupMode::Export => "导出",
                            DriverBackupMode::Import => "导入",
                        };

                        let can_execute = !self.driver_backup_path.is_empty()
                            && self.driver_backup_target.is_some()
                            && !is_loading_partitions;

                        if ui
                            .add_enabled(can_execute, egui::Button::new(button_label))
                            .clicked()
                        {
                            self.start_driver_backup_action();
                        }
                    }

                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_driver_backup_dialog = false;
        }
    }

    /// 启动后台驱动备份/还原操作
    fn start_driver_backup_action(&mut self) {
        if self.driver_backup_path.is_empty() {
            self.driver_backup_message = "请指定目录路径".to_string();
            return;
        }

        let target = match &self.driver_backup_target {
            Some(t) => t.clone(),
            None => {
                self.driver_backup_message = "请选择系统分区".to_string();
                return;
            }
        };

        let path = self.driver_backup_path.clone();
        let mode = self.driver_backup_mode;
        
        self.driver_backup_loading = true;
        self.driver_backup_message = match mode {
            DriverBackupMode::Export => "正在导出驱动，请稍候...".to_string(),
            DriverBackupMode::Import => "正在导入驱动，请稍候...".to_string(),
        };

        let (tx, rx) = mpsc::channel();
        self.driver_operation_rx = Some(rx);

        std::thread::spawn(move || {
            let dism = crate::core::dism::Dism::new();
            
            let result = match mode {
                DriverBackupMode::Export => {
                    match dism.export_drivers_from_system(&target, &path) {
                        Ok(_) => Ok(format!("驱动导出成功: {} -> {}", target, path)),
                        Err(e) => Err(format!("驱动导出失败: {}", e)),
                    }
                }
                DriverBackupMode::Import => {
                    // 检查驱动目录是否存在
                    if !std::path::Path::new(&path).exists() {
                        Err(format!("驱动目录不存在: {}", path))
                    } else {
                        match dism.add_drivers_offline(&target, &path) {
                            Ok(_) => Ok("驱动导入成功！".to_string()),
                            Err(e) => Err(format!("驱动导入失败: {}", e)),
                        }
                    }
                }
            };
            
            let _ = tx.send(result);
        });
    }

    /// 渲染软件列表对话框
    pub fn render_software_list_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_software_list_dialog {
            return;
        }

        let mut should_close = false;
        let mut save_path: Option<std::path::PathBuf> = None;
        
        // 克隆数据避免借用冲突
        let software_list_clone = self.software_list.clone();
        let is_loading = self.software_list_loading;

        egui::Window::new("已安装软件列表")
            .resizable(true)
            .default_width(500.0)
            .default_height(450.0)
            .show(ui.ctx(), |ui| {
                if is_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在加载软件列表...");
                    });
                } else {
                    ui.label(format!("共 {} 个软件", software_list_clone.len()));
                    ui.add_space(5.0);

                    // 表头
                    egui::Grid::new("software_header")
                        .num_columns(3)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("软件名称").strong());
                            ui.label(egui::RichText::new("版本").strong());
                            ui.label(egui::RichText::new("发布者").strong());
                            ui.end_row();
                        });

                    ui.separator();

                    // 软件列表
                    egui::ScrollArea::vertical()
                        .max_height(350.0)
                        .show(ui, |ui| {
                            egui::Grid::new("software_list")
                                .num_columns(3)
                                .spacing([8.0, 2.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    for software in &software_list_clone {
                                        ui.label(truncate_string(&software.name, 30));
                                        ui.label(truncate_string(&software.version, 15));
                                        ui.label(truncate_string(&software.publisher, 20));
                                        ui.end_row();
                                    }
                                });
                        });
                }

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("保存列表为TXT").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("installed_software.txt")
                            .add_filter("文本文件", &["txt"])
                            .save_file()
                        {
                            save_path = Some(path);
                        }
                    }

                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        // 在窗口渲染之后处理保存
        if let Some(path) = save_path {
            save_software_list_to_file(&path, &software_list_clone);
        }

        if should_close {
            self.show_software_list_dialog = false;
        }
    }

    /// 渲染重置网络确认对话框
    pub fn render_reset_network_confirm_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_reset_network_confirm_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_reset = false;

        egui::Window::new("确认重置网络设置")
            .resizable(false)
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(6.0);
                ui.label("此操作将执行以下命令重置网络设置：");
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
                ui.label("重置后可能需要重新配置网络连接。");
                ui.add_space(15.0);

                ui.horizontal(|ui| {
                    if ui.button("确认重置").clicked() {
                        do_reset = true;
                        should_close = true;
                    }
                    if ui.button("取消").clicked() {
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

        self.tool_message = format!(
            "网络重置完成: 成功 {} 个命令, 失败 {} 个命令",
            success_count, fail_count
        );

        if success_count > 0 {
            self.tool_message.push_str("\n建议重启计算机以完成网络重置。");
        }
    }

    /// 初始化网络信息对话框
    pub fn init_network_info_dialog(&mut self) {
        self.show_network_info_dialog = true;
        self.network_info_cache = Some(get_detailed_network_info());
    }

    /// 初始化软件列表对话框
    pub fn init_software_list_dialog(&mut self) {
        self.show_software_list_dialog = true;
        self.software_list_loading = true;
        self.software_list = get_installed_software();
        self.software_list_loading = false;
    }

    // ==================== 时间同步对话框 ====================
    
    /// 渲染时间同步对话框
    pub fn render_time_sync_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_time_sync_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_sync = false;

        egui::Window::new("系统时间校准")
            .resizable(false)
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(6.0);
                ui.label("是否立即网络同步本机的时间到北京时间？");
                ui.add_space(10.0);

                ui.label(egui::RichText::new("将从以下NTP服务器获取时间：").small());
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
                        ui.label("正在同步时间...");
                    } else {
                        if ui.button("确定").clicked() {
                            do_sync = true;
                        }
                        if ui.button("取消").clicked() {
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
        self.time_sync_message = "正在连接NTP服务器...".to_string();

        let (tx, rx) = mpsc::channel();
        self.time_sync_rx = Some(rx);

        std::thread::spawn(move || {
            let result = super::time_sync::sync_time_to_beijing();
            let _ = tx.send(result);
        });
    }

    // ==================== 批量格式化对话框 ====================

    /// 渲染批量格式化对话框
    pub fn render_batch_format_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_batch_format_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_format = false;

        egui::Window::new("批量格式化")
            .resizable(true)
            .default_width(500.0)
            .default_height(400.0)
            .show(ui.ctx(), |ui| {
                ui.label("选择要格式化的分区（系统盘已自动隐藏）");
                ui.add_space(10.0);

                if self.batch_format_partitions_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测分区...");
                    });
                } else if self.batch_format_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "未找到可格式化的分区",
                    );
                } else {
                    // 全选/反选按钮
                    ui.horizontal(|ui| {
                        if ui.button("全选").clicked() {
                            for p in &self.batch_format_partitions {
                                self.batch_format_selected.insert(p.letter.clone());
                            }
                        }
                        if ui.button("反选").clicked() {
                            let current: HashSet<_> = self.batch_format_selected.clone();
                            self.batch_format_selected.clear();
                            for p in &self.batch_format_partitions {
                                if !current.contains(&p.letter) {
                                    self.batch_format_selected.insert(p.letter.clone());
                                }
                            }
                        }
                        ui.label(format!("已选择 {} 个分区", self.batch_format_selected.len()));
                    });

                    ui.add_space(5.0);
                    ui.separator();

                    // 分区列表
                    egui::ScrollArea::vertical()
                        .max_height(250.0)
                        .show(ui, |ui| {
                            for partition in &self.batch_format_partitions.clone() {
                                let mut selected = self.batch_format_selected.contains(&partition.letter);
                                
                                let display_text = format!(
                                    "{} [{}] - {} ({:.1} GB / {:.1} GB 可用)",
                                    partition.letter,
                                    if partition.label.is_empty() { "无标签" } else { &partition.label },
                                    partition.file_system,
                                    partition.total_size_mb as f64 / 1024.0,
                                    partition.free_size_mb as f64 / 1024.0,
                                );

                                if ui.checkbox(&mut selected, display_text).changed() {
                                    if selected {
                                        self.batch_format_selected.insert(partition.letter.clone());
                                    } else {
                                        self.batch_format_selected.remove(&partition.letter);
                                    }
                                }
                            }
                        });
                }

                ui.add_space(10.0);

                // 显示状态消息
                if !self.batch_format_message.is_empty() {
                    let color = get_message_color(&self.batch_format_message);
                    ui.colored_label(color, &self.batch_format_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.batch_format_loading {
                        ui.spinner();
                        ui.label("正在格式化...");
                    } else {
                        let can_format = !self.batch_format_selected.is_empty()
                            && !self.batch_format_partitions_loading;

                        if ui
                            .add_enabled(can_format, egui::Button::new("应用（格式化选中分区）"))
                            .clicked()
                        {
                            // 显示确认对话框
                            do_format = true;
                        }

                        if ui.button("刷新").clicked() {
                            self.start_load_formatable_partitions();
                        }

                        if ui.button("关闭").clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_format && !self.batch_format_selected.is_empty() {
            // 开始格式化
            self.start_batch_format();
        }

        if should_close {
            self.show_batch_format_dialog = false;
        }
    }

    /// 启动后台加载可格式化分区
    pub fn start_load_formatable_partitions(&mut self) {
        if self.batch_format_partitions_loading {
            return;
        }

        self.batch_format_partitions_loading = true;
        self.batch_format_partitions.clear();

        let (tx, rx) = mpsc::channel();
        self.batch_format_partitions_rx = Some(rx);

        std::thread::spawn(move || {
            let partitions = super::batch_format::get_formatable_partitions();
            let _ = tx.send(partitions);
        });
    }

    /// 启动后台批量格式化
    fn start_batch_format(&mut self) {
        if self.batch_format_loading {
            return;
        }

        self.batch_format_loading = true;
        self.batch_format_message = "正在格式化分区...".to_string();

        let selected: Vec<String> = self.batch_format_selected.iter().cloned().collect();
        let (tx, rx) = mpsc::channel();
        self.batch_format_rx = Some(rx);

        std::thread::spawn(move || {
            let result = super::batch_format::batch_format_partitions(&selected, "新加卷", "NTFS");
            let _ = tx.send(result);
        });
    }

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

        egui::Window::new("BitLocker管理")
            .resizable(true)
            .default_width(560.0)
            .default_height(420.0)
            .show(ui.ctx(), |ui| {
                ui.label("管理本机 BitLocker 加密分区：解锁已锁定的分区，或彻底关闭（解密）已解锁的分区。");
                ui.add_space(10.0);

                if self.bitlocker_manage_partitions_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测 BitLocker 分区...");
                    });
                } else if self.bitlocker_manage_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "未检测到 BitLocker 加密分区",
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
                                    ui.label(egui::RichText::new("选择").strong());
                                    ui.label(egui::RichText::new("分区").strong());
                                    ui.label(egui::RichText::new("大小").strong());
                                    ui.label(egui::RichText::new("卷标").strong());
                                    ui.label(egui::RichText::new("状态").strong());
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
                                                format!("{} ({}%)", partition.status.as_str(), p)
                                            }
                                            _ => partition.status.as_str().to_string(),
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
                                ui.label("解锁方式:");
                                ui.radio_value(&mut self.bitlocker_manage_mode, BitLockerUnlockMode::Password, "密码");
                                ui.radio_value(&mut self.bitlocker_manage_mode, BitLockerUnlockMode::RecoveryKey, "恢复密钥");
                            });
                            ui.add_space(5.0);
                            match self.bitlocker_manage_mode {
                                BitLockerUnlockMode::Password => {
                                    ui.horizontal(|ui| {
                                        ui.label("密码:");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.bitlocker_manage_password)
                                                .password(true)
                                                .desired_width(320.0),
                                        );
                                    });
                                }
                                BitLockerUnlockMode::RecoveryKey => {
                                    ui.horizontal(|ui| {
                                        ui.label("恢复密钥:");
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
                                "该分区已解锁，可彻底关闭 BitLocker（解密）",
                            );
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 165, 0),
                                "解密在后台进行，可能耗时较长，期间请勿断电或重启。",
                            );
                        }
                        Some(VolumeStatus::Decrypting) => {
                            ui.add_space(5.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 150, 255),
                                "该分区正在解密中，请等待完成。",
                            );
                        }
                        Some(VolumeStatus::Encrypting) => {
                            ui.add_space(5.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 150, 255),
                                "该分区正在加密中。",
                            );
                        }
                        _ => {
                            ui.add_space(5.0);
                            ui.label("请选择一个分区进行操作。");
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
                        "恢复密钥（48 位数字），请妥善保管、勿泄露：",
                    );
                    ui.monospace(key.as_str());
                    ui.horizontal(|ui| {
                        if ui.button("导出到文件").clicked() {
                            do_export_recovery = true;
                        }
                        if ui.button("隐藏").clicked() {
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
                        ui.label("正在执行操作...");
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
                                if ui.add_enabled(can_unlock, egui::Button::new("解锁")).clicked() {
                                    do_unlock = true;
                                }
                            }
                            Some(VolumeStatus::EncryptedUnlocked) => {
                                if ui.button("关闭 BitLocker（解密）").clicked() {
                                    do_decrypt = true;
                                }
                                if ui.button("查看恢复密钥").clicked() {
                                    do_get_recovery = true;
                                }
                                if ui.button("挂起保护").clicked() {
                                    do_suspend = true;
                                }
                                if ui.button("恢复保护").clicked() {
                                    do_resume = true;
                                }
                            }
                            _ => {}
                        }

                        if ui.button("刷新").clicked() {
                            do_refresh = true;
                        }
                        if ui.button("关闭").clicked() {
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
                                format!("恢复密钥已导出到 {}", path.display())
                        }
                        Err(e) => self.bitlocker_manage_message = format!("导出失败: {}", e),
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
            let partitions = super::bitlocker::get_bitlocker_partitions();
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
                self.bitlocker_manage_message = "请先选择要解锁的分区".to_string();
                return;
            }
        };

        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message = "正在解锁...".to_string();

        let mode = self.bitlocker_manage_mode;
        let password = self.bitlocker_manage_password.clone();
        let recovery_key = self.bitlocker_manage_recovery_key.clone();

        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_unlock_rx = Some(rx);

        std::thread::spawn(move || {
            let result = match mode {
                BitLockerUnlockMode::Password => {
                    super::bitlocker::unlock_with_password(&drive, &password)
                }
                BitLockerUnlockMode::RecoveryKey => {
                    super::bitlocker::unlock_with_recovery_key(&drive, &recovery_key)
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
                self.bitlocker_manage_message = "请先选择要解密的分区".to_string();
                return;
            }
        };

        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message = "正在发起解密...".to_string();

        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_decrypt_rx = Some(rx);

        std::thread::spawn(move || {
            let result = super::bitlocker::decrypt_partition(&drive);
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
                self.bitlocker_manage_message = "请先选择分区".to_string();
                return;
            }
        };
        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message = "正在读取恢复密钥...".to_string();
        self.bitlocker_manage_recovery_display = None;
        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_recovery_rx = Some(rx);
        std::thread::spawn(move || {
            let result = super::bitlocker::get_recovery_key_partition(&drive);
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
                self.bitlocker_manage_message = "请先选择分区".to_string();
                return;
            }
        };
        self.bitlocker_manage_loading = true;
        self.bitlocker_manage_message =
            if suspend { "正在挂起保护..." } else { "正在恢复保护..." }.to_string();
        let (tx, rx) = mpsc::channel();
        self.bitlocker_manage_protect_rx = Some(rx);
        std::thread::spawn(move || {
            let result = if suspend {
                super::bitlocker::suspend_partition_protection(&drive)
            } else {
                super::bitlocker::resume_partition_protection(&drive)
            };
            let _ = tx.send(result);
        });
    }

    /// 检查 BitLocker 管理工具的异步操作结果
    fn check_bitlocker_manage_async_operations(&mut self) {
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
                    self.bitlocker_manage_message = format!("{} 解锁成功", result.letter);
                    self.bitlocker_manage_password.clear();
                    self.bitlocker_manage_recovery_key.clear();
                    // 刷新列表以更新状态
                    self.start_load_bitlocker_manage_partitions();
                } else {
                    self.bitlocker_manage_message =
                        format!("{} 解锁失败: {}", result.letter, result.message);
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
                        format!("{} 解密失败: {}", result.letter, result.message);
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
                        self.bitlocker_manage_message = "已读取恢复密钥".to_string();
                    }
                    Err(e) => {
                        self.bitlocker_manage_message = format!("读取恢复密钥失败: {}", e);
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
                        self.bitlocker_manage_message = format!("操作失败: {}", e);
                    }
                }
            }
        }
    }

    // ==================== 分区对拷对话框 ====================

    /// 检查分区对拷异步操作结果
    fn check_partition_copy_async_operations(&mut self) {
        // 检查分区列表加载结果
        if let Some(ref rx) = self.partition_copy_partitions_rx {
            if let Ok(partitions) = rx.try_recv() {
                self.partition_copy_partitions = partitions;
                self.partition_copy_partitions_loading = false;
                self.partition_copy_partitions_rx = None;
                
                // 自动检查是否可以继续对拷
                self.update_partition_copy_resume_state();
            }
        }
        
        // 检查复制进度
        if let Some(ref rx) = self.partition_copy_progress_rx {
            // 使用 try_iter 获取所有可用的进度更新
            let mut latest_progress: Option<super::partition_copy::CopyProgress> = None;
            
            while let Ok(progress) = rx.try_recv() {
                latest_progress = Some(progress);
            }
            
            if let Some(progress) = latest_progress {
                // 更新日志
                if !progress.current_file.is_empty() && !progress.current_file.starts_with("正在") {
                    // 添加到日志（限制日志长度）
                    let log_line = if progress.completed {
                        format!("[完成] {}\n", progress.current_file)
                    } else {
                        format!("[复制] {}\n", progress.current_file)
                    };
                    self.partition_copy_log.push_str(&log_line);
                    
                    // 限制日志长度，保留最新的部分
                    const MAX_LOG_BYTES: usize = 100_000;
                    if self.partition_copy_log.len() > MAX_LOG_BYTES {
                        // 找到合适的截断点
                        let start = self.partition_copy_log.len() - MAX_LOG_BYTES / 2;
                        if let Some(newline_pos) = self.partition_copy_log[start..].find('\n') {
                            self.partition_copy_log = self.partition_copy_log[start + newline_pos + 1..].to_string();
                        }
                    }
                }
                
                // 更新消息
                if progress.completed {
                    let msg = if progress.failed_count > 0 {
                        format!(
                            "复制完成！已复制 {} 个文件，跳过 {} 个，失败 {} 个",
                            progress.copied_count,
                            progress.skipped_count,
                            progress.failed_count
                        )
                    } else {
                        format!(
                            "复制完成！已复制 {} 个文件，跳过 {} 个（已存在）",
                            progress.copied_count,
                            progress.skipped_count
                        )
                    };
                    self.partition_copy_message = msg;
                    self.partition_copy_copying = false;
                    self.partition_copy_progress_rx = None;
                    
                    // 刷新分区列表
                    self.start_load_copyable_partitions();
                } else if let Some(ref error) = progress.error {
                    self.partition_copy_message = format!("错误: {}", error);
                    self.partition_copy_copying = false;
                    self.partition_copy_progress_rx = None;
                } else {
                    self.partition_copy_message = format!(
                        "正在复制 {}/{}（跳过 {}）: {}",
                        progress.copied_count,
                        progress.total_count,
                        progress.skipped_count,
                        progress.current_file
                    );
                }
                
                self.partition_copy_progress = Some(progress);
            }
        }
    }

    /// 更新是否可以继续对拷的状态
    fn update_partition_copy_resume_state(&mut self) {
        if let (Some(source), Some(target)) = (&self.partition_copy_source, &self.partition_copy_target) {
            self.partition_copy_is_resume = super::partition_copy::can_resume_copy(source, target);
        } else {
            self.partition_copy_is_resume = false;
        }
    }

    /// 渲染分区对拷对话框
    pub fn render_partition_copy_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_partition_copy_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_copy = false;

        egui::Window::new("分区对拷")
            .resizable(true)
            .default_width(650.0)
            .default_height(550.0)
            .show(ui.ctx(), |ui| {
                ui.label("将源分区的所有文件复制到目标分区（支持断点续传）");
                ui.add_space(10.0);

                if self.partition_copy_partitions_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测分区...");
                    });
                } else if self.partition_copy_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "未找到可用的分区",
                    );
                } else {
                    // 克隆分区列表避免借用冲突
                    let partitions_clone = self.partition_copy_partitions.clone();
                    
                    // ========== 源分区选择 ==========
                    ui.horizontal(|ui| {
                        ui.label("请选择源分区:");
                        let current_source = self.partition_copy_source.clone().unwrap_or_else(|| "请选择".to_string());
                        
                        egui::ComboBox::from_id_salt("partition_copy_source")
                            .selected_text(&current_source)
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for partition in &partitions_clone {
                                    let display = format!("{}", partition.letter);
                                    ui.selectable_value(
                                        &mut self.partition_copy_source,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });

                    ui.add_space(5.0);

                    // 源分区列表框
                    ui.group(|ui| {
                        ui.set_min_height(120.0);
                        ui.set_max_height(120.0);
                        
                        egui::ScrollArea::vertical()
                            .id_salt("source_partition_scroll")
                            .show(ui, |ui| {
                                // 表头
                                egui::Grid::new("source_partition_header")
                                    .num_columns(5)
                                    .spacing([10.0, 4.0])
                                    .min_col_width(80.0)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new("分区卷").strong());
                                        ui.label(egui::RichText::new("总空间").strong());
                                        ui.label(egui::RichText::new("已用空间").strong());
                                        ui.label(egui::RichText::new("卷标").strong());
                                        ui.label(egui::RichText::new("状态").strong());
                                        ui.end_row();
                                    });

                                ui.separator();

                                // 分区列表
                                egui::Grid::new("source_partition_list")
                                    .num_columns(5)
                                    .spacing([10.0, 2.0])
                                    .min_col_width(80.0)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for partition in &partitions_clone {
                                            let is_selected = self.partition_copy_source.as_ref() == Some(&partition.letter);
                                            
                                            if ui.selectable_label(is_selected, &partition.letter).clicked() {
                                                self.partition_copy_source = Some(partition.letter.clone());
                                                self.update_partition_copy_resume_state();
                                            }
                                            
                                            ui.label(format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0));
                                            ui.label(format!("{:.1} GB", partition.used_size_mb as f64 / 1024.0));
                                            ui.label(if partition.label.is_empty() { "-" } else { &partition.label });
                                            ui.label(if partition.has_system { "有系统" } else { "无系统" });
                                            ui.end_row();
                                        }
                                    });
                            });
                    });

                    ui.add_space(15.0);

                    // ========== 目标分区选择 ==========
                    ui.horizontal(|ui| {
                        ui.label("请选择目标分区:");
                        let current_target = self.partition_copy_target.clone().unwrap_or_else(|| "请选择".to_string());
                        
                        egui::ComboBox::from_id_salt("partition_copy_target")
                            .selected_text(&current_target)
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for partition in &partitions_clone {
                                    let display = format!("{}", partition.letter);
                                    ui.selectable_value(
                                        &mut self.partition_copy_target,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });

                    ui.add_space(5.0);

                    // 目标分区列表框
                    ui.group(|ui| {
                        ui.set_min_height(120.0);
                        ui.set_max_height(120.0);
                        
                        egui::ScrollArea::vertical()
                            .id_salt("target_partition_scroll")
                            .show(ui, |ui| {
                                // 表头
                                egui::Grid::new("target_partition_header")
                                    .num_columns(5)
                                    .spacing([10.0, 4.0])
                                    .min_col_width(80.0)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new("分区卷").strong());
                                        ui.label(egui::RichText::new("总空间").strong());
                                        ui.label(egui::RichText::new("已用空间").strong());
                                        ui.label(egui::RichText::new("卷标").strong());
                                        ui.label(egui::RichText::new("状态").strong());
                                        ui.end_row();
                                    });

                                ui.separator();

                                // 分区列表
                                egui::Grid::new("target_partition_list")
                                    .num_columns(5)
                                    .spacing([10.0, 2.0])
                                    .min_col_width(80.0)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for partition in &partitions_clone {
                                            let is_selected = self.partition_copy_target.as_ref() == Some(&partition.letter);
                                            
                                            if ui.selectable_label(is_selected, &partition.letter).clicked() {
                                                self.partition_copy_target = Some(partition.letter.clone());
                                                self.update_partition_copy_resume_state();
                                            }
                                            
                                            ui.label(format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0));
                                            ui.label(format!("{:.1} GB", partition.used_size_mb as f64 / 1024.0));
                                            ui.label(if partition.label.is_empty() { "-" } else { &partition.label });
                                            ui.label(if partition.has_system { "有系统" } else { "无系统" });
                                            ui.end_row();
                                        }
                                    });
                            });
                    });
                }

                ui.add_space(15.0);

                // 显示复制日志（如果正在复制或已复制）
                if self.partition_copy_copying || !self.partition_copy_log.is_empty() {
                    ui.label("复制日志:");
                    ui.group(|ui| {
                        ui.set_min_height(100.0);
                        ui.set_max_height(100.0);
                        
                        egui::ScrollArea::vertical()
                            .id_salt("partition_copy_log")
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.partition_copy_log.as_str())
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .interactive(false)
                                );
                            });
                    });
                    ui.add_space(10.0);
                }

                // 显示状态消息
                if !self.partition_copy_message.is_empty() {
                    let color = get_message_color(&self.partition_copy_message);
                    ui.colored_label(color, &self.partition_copy_message);
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if self.partition_copy_copying {
                        ui.spinner();
                        ui.label("正在复制...");
                    } else {
                        // 检查是否可以开始复制
                        let source_valid = self.partition_copy_source.is_some();
                        let target_valid = self.partition_copy_target.is_some();
                        let same_partition = source_valid && target_valid 
                            && self.partition_copy_source == self.partition_copy_target;
                        
                        let can_copy = source_valid && target_valid && !same_partition
                            && !self.partition_copy_partitions_loading;

                        // 根据是否可以继续显示不同的按钮文字
                        let button_text = if self.partition_copy_is_resume {
                            "继续对拷"
                        } else {
                            "开始对拷"
                        };

                        if ui
                            .add_enabled(can_copy, egui::Button::new(button_text))
                            .clicked()
                        {
                            if same_partition {
                                self.partition_copy_message = "错误: 源分区和目标分区不能相同！".to_string();
                            } else {
                                do_copy = true;
                            }
                        }

                        // 如果选择了相同分区，显示错误提示
                        if same_partition {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 80, 80),
                                "源分区和目标分区不能相同！"
                            );
                        }

                        if ui.button("刷新").clicked() {
                            self.start_load_copyable_partitions();
                        }

                        if ui.button("关闭").clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        if do_copy {
            self.start_partition_copy();
        }

        if should_close {
            self.show_partition_copy_dialog = false;
        }
    }

    /// 启动后台加载可复制分区列表
    pub fn start_load_copyable_partitions(&mut self) {
        if self.partition_copy_partitions_loading {
            return;
        }

        self.partition_copy_partitions_loading = true;
        self.partition_copy_partitions.clear();

        let (tx, rx) = mpsc::channel();
        self.partition_copy_partitions_rx = Some(rx);

        std::thread::spawn(move || {
            let partitions = super::partition_copy::get_copyable_partitions();
            let _ = tx.send(partitions);
        });
    }

    /// 启动分区对拷操作
    fn start_partition_copy(&mut self) {
        let source = match &self.partition_copy_source {
            Some(s) => s.clone(),
            None => {
                self.partition_copy_message = "请选择源分区".to_string();
                return;
            }
        };

        let target = match &self.partition_copy_target {
            Some(t) => t.clone(),
            None => {
                self.partition_copy_message = "请选择目标分区".to_string();
                return;
            }
        };

        if source == target {
            self.partition_copy_message = "错误: 源分区和目标分区不能相同！".to_string();
            return;
        }

        // 检查目标空间
        if let Err(e) = super::partition_copy::check_target_space(&source, &target) {
            self.partition_copy_message = e;
            return;
        }

        self.partition_copy_copying = true;
        self.partition_copy_log.clear();
        self.partition_copy_message = "正在准备复制...".to_string();

        let is_resume = self.partition_copy_is_resume;
        
        let (tx, rx) = mpsc::channel();
        self.partition_copy_progress_rx = Some(rx);

        std::thread::spawn(move || {
            super::partition_copy::execute_partition_copy(&source, &target, tx, is_resume);
        });
    }

    // ==================== 安装时BitLocker解锁对话框 ====================

    /// 渲染安装时BitLocker解锁对话框
    pub fn render_install_bitlocker_dialog(&mut self, ui: &mut egui::Ui) {
        use crate::app::BitLockerUnlockMode;
        use crate::core::bitlocker::VolumeStatus;

        if !self.show_install_bitlocker_dialog {
            return;
        }

        // 检查解锁结果
        self.check_install_bitlocker_unlock_result();

        let mut should_close = false;
        let mut do_unlock = false;
        let mut do_skip = false;
        let mut do_skip_all = false;

        egui::Window::new("BitLocker解锁")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.set_min_width(500.0);
                
                ui.label("检测到以下分区被BitLocker加密锁定，需要解锁后才能继续安装：");
                ui.add_space(10.0);

                // 显示锁定分区列表
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .show(ui, |ui| {
                        egui::Grid::new("install_bitlocker_partitions")
                            .num_columns(4)
                            .spacing([10.0, 4.0])
                            .min_col_width(80.0)
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("分区").strong());
                                ui.label(egui::RichText::new("大小").strong());
                                ui.label(egui::RichText::new("卷标").strong());
                                ui.label(egui::RichText::new("状态").strong());
                                ui.end_row();

                                for partition in &self.install_bitlocker_partitions {
                                    let is_current = self.install_bitlocker_current.as_ref() == Some(&partition.letter);
                                    
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
                                    ui.colored_label(status_color, partition.status.as_str());
                                    ui.end_row();
                                }
                            });
                    });

                ui.add_space(10.0);
                ui.separator();

                // 检查是否还有需要解锁的分区
                let has_locked = self.install_bitlocker_partitions.iter()
                    .any(|p| p.status == VolumeStatus::EncryptedLocked);

                if has_locked {
                    // 显示当前要解锁的分区
                    if let Some(ref current) = self.install_bitlocker_current {
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            ui.label("当前解锁:");
                            ui.strong(current);
                        });
                    }

                    ui.add_space(10.0);

                    // 解锁模式选择
                    ui.horizontal(|ui| {
                        ui.label("解锁方式:");
                        ui.radio_value(&mut self.install_bitlocker_mode, BitLockerUnlockMode::Password, "密码");
                        ui.radio_value(&mut self.install_bitlocker_mode, BitLockerUnlockMode::RecoveryKey, "恢复密钥");
                    });

                    ui.add_space(5.0);

                    // 输入框
                    match self.install_bitlocker_mode {
                        BitLockerUnlockMode::Password => {
                            ui.horizontal(|ui| {
                                ui.label("密码:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.install_bitlocker_password)
                                        .password(true)
                                        .desired_width(300.0),
                                );
                            });
                        }
                        BitLockerUnlockMode::RecoveryKey => {
                            ui.horizontal(|ui| {
                                ui.label("恢复密钥:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.install_bitlocker_recovery_key)
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
                        "所有分区已解锁，可以继续安装",
                    );
                }

                // 显示消息
                if !self.install_bitlocker_message.is_empty() {
                    ui.add_space(10.0);
                    let color = get_message_color(&self.install_bitlocker_message);
                    ui.colored_label(color, &self.install_bitlocker_message);
                }

                ui.add_space(15.0);
                ui.separator();
                ui.add_space(5.0);

                // 按钮
                ui.horizontal(|ui| {
                    if self.install_bitlocker_loading {
                        ui.spinner();
                        ui.label("正在解锁...");
                    } else if has_locked {
                        let can_unlock = self.install_bitlocker_current.is_some()
                            && match self.install_bitlocker_mode {
                                BitLockerUnlockMode::Password => !self.install_bitlocker_password.is_empty(),
                                BitLockerUnlockMode::RecoveryKey => !self.install_bitlocker_recovery_key.is_empty(),
                            };

                        if ui.add_enabled(can_unlock, egui::Button::new("解锁")).clicked() {
                            do_unlock = true;
                        }

                        if ui.button("跳过此分区").clicked() {
                            do_skip = true;
                        }

                        if ui.button("跳过所有").clicked() {
                            do_skip_all = true;
                        }

                        if ui.button("取消安装").clicked() {
                            should_close = true;
                        }
                    } else {
                        // 所有分区都已解锁
                        if ui.button("继续安装").clicked() {
                            should_close = true;
                            if self.install_bitlocker_continue_after {
                                self.continue_installation_after_bitlocker();
                            }
                        }

                        if ui.button("取消").clicked() {
                            should_close = true;
                        }
                    }
                });
            });

        // 处理操作
        if do_unlock {
            self.start_install_bitlocker_unlock();
        }

        if do_skip {
            self.skip_current_install_bitlocker_partition();
        }

        if do_skip_all {
            // 跳过所有锁定的分区
            self.install_bitlocker_partitions.retain(|p| p.status != VolumeStatus::EncryptedLocked);
            self.install_bitlocker_current = None;
            self.install_bitlocker_message = "已跳过所有锁定的分区".to_string();
        }

        if should_close {
            self.show_install_bitlocker_dialog = false;
            self.install_bitlocker_continue_after = false;
        }
    }

    /// 检查安装时BitLocker解锁结果
    fn check_install_bitlocker_unlock_result(&mut self) {
        use crate::core::bitlocker::VolumeStatus;

        if let Some(ref rx) = self.install_bitlocker_rx {
            if let Ok(result) = rx.try_recv() {
                self.install_bitlocker_loading = false;
                self.install_bitlocker_rx = None;

                if result.success {
                    self.install_bitlocker_message = format!("{} 解锁成功", result.letter);
                    
                    // 更新分区状态
                    if let Some(partition) = self.install_bitlocker_partitions.iter_mut()
                        .find(|p| p.letter == result.letter)
                    {
                        partition.status = VolumeStatus::EncryptedUnlocked;
                    }

                    // 清空输入
                    self.install_bitlocker_password.clear();
                    self.install_bitlocker_recovery_key.clear();

                    // 选择下一个需要解锁的分区
                    self.select_next_install_bitlocker_partition();
                } else {
                    self.install_bitlocker_message = format!("{} 解锁失败: {}", result.letter, result.message);
                }
            }
        }
    }

    /// 启动安装时BitLocker解锁
    fn start_install_bitlocker_unlock(&mut self) {
        use crate::app::BitLockerUnlockMode;

        if self.install_bitlocker_loading {
            return;
        }

        let drive = match &self.install_bitlocker_current {
            Some(d) => d.clone(),
            None => {
                self.install_bitlocker_message = "请先选择要解锁的分区".to_string();
                return;
            }
        };

        self.install_bitlocker_loading = true;
        self.install_bitlocker_message = "正在解锁...".to_string();

        let mode = self.install_bitlocker_mode;
        let password = self.install_bitlocker_password.clone();
        let recovery_key = self.install_bitlocker_recovery_key.clone();

        let (tx, rx) = mpsc::channel();
        self.install_bitlocker_rx = Some(rx);

        std::thread::spawn(move || {
            let result = match mode {
                BitLockerUnlockMode::Password => {
                    super::bitlocker::unlock_with_password(&drive, &password)
                }
                BitLockerUnlockMode::RecoveryKey => {
                    super::bitlocker::unlock_with_recovery_key(&drive, &recovery_key)
                }
            };
            let _ = tx.send(result);
        });
    }

    /// 跳过当前安装时BitLocker分区
    fn skip_current_install_bitlocker_partition(&mut self) {

        if let Some(ref current) = self.install_bitlocker_current.clone() {
            // 从列表中移除当前分区
            self.install_bitlocker_partitions.retain(|p| p.letter != *current);
            self.install_bitlocker_message = format!("已跳过分区 {}", current);
            
            // 选择下一个需要解锁的分区
            self.select_next_install_bitlocker_partition();
        }
    }

    /// 选择下一个需要解锁的安装时BitLocker分区
    fn select_next_install_bitlocker_partition(&mut self) {
        use crate::core::bitlocker::VolumeStatus;

        self.install_bitlocker_current = self.install_bitlocker_partitions
            .iter()
            .find(|p| p.status == VolumeStatus::EncryptedLocked)
            .map(|p| p.letter.clone());
    }
    
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

        egui::Window::new("BitLocker解锁 - 备份")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.set_min_width(500.0);
                
                ui.label("检测到以下分区被BitLocker加密锁定，需要解锁后才能继续备份：");
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
                                ui.label(egui::RichText::new("分区").strong());
                                ui.label(egui::RichText::new("大小").strong());
                                ui.label(egui::RichText::new("卷标").strong());
                                ui.label(egui::RichText::new("状态").strong());
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
                                    ui.colored_label(status_color, partition.status.as_str());
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
                            ui.label("当前解锁:");
                            ui.strong(current);
                        });
                    }

                    ui.add_space(10.0);

                    // 解锁模式选择
                    ui.horizontal(|ui| {
                        ui.label("解锁方式:");
                        ui.radio_value(&mut self.backup_bitlocker_mode, BitLockerUnlockMode::Password, "密码");
                        ui.radio_value(&mut self.backup_bitlocker_mode, BitLockerUnlockMode::RecoveryKey, "恢复密钥");
                    });

                    ui.add_space(5.0);

                    // 输入框
                    match self.backup_bitlocker_mode {
                        BitLockerUnlockMode::Password => {
                            ui.horizontal(|ui| {
                                ui.label("密码:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.backup_bitlocker_password)
                                        .password(true)
                                        .desired_width(300.0),
                                );
                            });
                        }
                        BitLockerUnlockMode::RecoveryKey => {
                            ui.horizontal(|ui| {
                                ui.label("恢复密钥:");
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
                        "所有分区已解锁，可以继续备份",
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
                        ui.label("正在解锁...");
                    } else if has_locked {
                        let can_unlock = self.backup_bitlocker_current.is_some()
                            && match self.backup_bitlocker_mode {
                                BitLockerUnlockMode::Password => !self.backup_bitlocker_password.is_empty(),
                                BitLockerUnlockMode::RecoveryKey => !self.backup_bitlocker_recovery_key.is_empty(),
                            };

                        if ui.add_enabled(can_unlock, egui::Button::new("解锁")).clicked() {
                            do_unlock = true;
                        }

                        if ui.button("跳过此分区").clicked() {
                            do_skip = true;
                        }

                        if ui.button("跳过所有").clicked() {
                            do_skip_all = true;
                        }

                        if ui.button("取消备份").clicked() {
                            should_close = true;
                        }
                    } else {
                        // 所有分区都已解锁
                        if ui.button("继续备份").clicked() {
                            should_close = true;
                            if self.backup_bitlocker_continue_after {
                                self.continue_backup_after_bitlocker();
                            }
                        }

                        if ui.button("取消").clicked() {
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
            self.backup_bitlocker_message = "已跳过所有锁定的分区".to_string();
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
                    self.backup_bitlocker_message = format!("{} 解锁成功", result.letter);
                    
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
                    self.backup_bitlocker_message = format!("{} 解锁失败: {}", result.letter, result.message);
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
                self.backup_bitlocker_message = "请先选择要解锁的分区".to_string();
                return;
            }
        };

        self.backup_bitlocker_loading = true;
        self.backup_bitlocker_message = "正在解锁...".to_string();

        let mode = self.backup_bitlocker_mode;
        let password = self.backup_bitlocker_password.clone();
        let recovery_key = self.backup_bitlocker_recovery_key.clone();

        let (tx, rx) = mpsc::channel();
        self.backup_bitlocker_rx = Some(rx);

        std::thread::spawn(move || {
            let result = match mode {
                BitLockerUnlockMode::Password => {
                    super::bitlocker::unlock_with_password(&drive, &password)
                }
                BitLockerUnlockMode::RecoveryKey => {
                    super::bitlocker::unlock_with_recovery_key(&drive, &recovery_key)
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
            self.backup_bitlocker_message = format!("已跳过分区 {}", current);
            
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

    // ==================== 一键修复引导对话框 ====================

    /// 渲染一键修复引导对话框
    pub fn render_repair_boot_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_repair_boot_dialog {
            return;
        }

        let mut should_close = false;
        let mut do_repair = false;
        let windows_partitions = self.get_cached_windows_partitions();
        let is_loading_partitions = self.windows_partitions_loading;

        egui::Window::new("一键修复引导")
            .resizable(false)
            .default_width(450.0)
            .show(ui.ctx(), |ui| {
                ui.label("修复Windows系统的启动引导");
                ui.add_space(10.0);

                // 分区选择
                if is_loading_partitions {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在检测Windows分区...");
                    });
                } else if windows_partitions.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 100, 100),
                        "未检测到包含Windows系统的分区",
                    );
                    ui.add_space(5.0);
                    ui.label("请确保目标分区包含有效的Windows系统");
                } else {
                    ui.horizontal(|ui| {
                        ui.label("选择目标系统分区:");

                        let current_text = self
                            .repair_boot_selected_partition
                            .as_ref()
                            .map(|letter| format_partition_display(&windows_partitions, letter))
                            .unwrap_or_else(|| "请选择".to_string());

                        egui::ComboBox::from_id_salt("repair_boot_partition_select")
                            .selected_text(current_text)
                            .width(250.0)
                            .show_ui(ui, |ui| {
                                for partition in &windows_partitions {
                                    let display = format!(
                                        "{} [{}] [{}]",
                                        partition.letter,
                                        partition.windows_version,
                                        partition.architecture
                                    );
                                    ui.selectable_value(
                                        &mut self.repair_boot_selected_partition,
                                        Some(partition.letter.clone()),
                                        display,
                                    );
                                }
                            });
                    });

                    // 显示所选分区的详细信息
                    if let Some(ref selected) = self.repair_boot_selected_partition {
                        if let Some(partition) = windows_partitions.iter().find(|p| &p.letter == selected) {
                            ui.add_space(10.0);
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Windows版本:");
                                    ui.label(&partition.windows_version);
                                });
                                ui.horizontal(|ui| {
                                    ui.label("系统架构:");
                                    ui.label(&partition.architecture);
                                });
                            });
                        }
                    }
                }

                ui.add_space(15.0);

                // 消息显示
                if !self.repair_boot_message.is_empty() {
                    let color = get_message_color(&self.repair_boot_message);
                    ui.colored_label(color, &self.repair_boot_message);
                    ui.add_space(10.0);
                }

                // 进度指示
                if self.repair_boot_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在修复引导...");
                    });
                    ui.add_space(10.0);
                }

                ui.separator();
                ui.add_space(5.0);

                // 按钮
                ui.horizontal(|ui| {
                    let can_repair = !self.repair_boot_loading 
                        && self.repair_boot_selected_partition.is_some()
                        && !windows_partitions.is_empty();

                    if ui
                        .add_enabled(can_repair, egui::Button::new("开始修复"))
                        .clicked()
                    {
                        do_repair = true;
                    }

                    if ui
                        .add_enabled(!self.repair_boot_loading, egui::Button::new("刷新"))
                        .clicked()
                    {
                        self.refresh_windows_partitions_cache();
                    }

                    if ui.button("关闭").clicked() {
                        should_close = true;
                    }
                });
            });

        // 执行修复
        if do_repair {
            self.repair_boot_action();
        }

        // 关闭对话框
        if should_close {
            self.show_repair_boot_dialog = false;
            self.repair_boot_message.clear();
            self.repair_boot_selected_partition = None;
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

/// 根据消息内容获取颜色
fn get_message_color(message: &str) -> egui::Color32 {
    if message.contains("成功") {
        egui::Color32::from_rgb(0, 180, 0)
    } else if message.contains("失败") || message.contains("错误") || message.contains("不存在") {
        egui::Color32::from_rgb(255, 80, 80)
    } else {
        egui::Color32::GRAY
    }
}
