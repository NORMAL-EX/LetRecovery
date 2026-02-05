//! 工具箱模块
//!
//! 提供各种系统维护和修复工具

pub mod types;
pub mod version_detect;
pub mod network;
pub mod driver;
pub mod appx;
pub mod software;
pub mod actions;
pub mod dialogs;
pub mod time_sync;
pub mod batch_format;
pub mod bitlocker;
pub mod gho_password;
pub mod nvidia_uninstall;
pub mod partition_copy;
pub mod quick_partition;
pub mod image_verify;

// 重新导出常用类型
pub use types::{DriverBackupMode, AppxPackageInfo, InstalledSoftware, WindowsPartitionInfo, ImageVerifyResult};
pub use batch_format::FormatablePartition;
pub use bitlocker::BitLockerPartition;
pub use partition_copy::{CopyablePartition, CopyProgress};
pub use quick_partition::QuickPartitionDialogState;

use egui;

use crate::app::App;

impl App {
    /// 显示工具箱页面
    pub fn show_tools(&mut self, ui: &mut egui::Ui) {
        ui.heading("工具箱");
        ui.separator();

        let is_pe = self
            .system_info
            .as_ref()
            .map(|s| s.is_pe_environment)
            .unwrap_or(false);

        ui.label("常用工具");
        ui.add_space(10.0);

        egui::Grid::new("tools_grid")
            .num_columns(4)
            .spacing([15.0, 12.0])
            .show(ui, |ui| {
                let button_size = egui::vec2(130.0, 50.0);

                // ========== 第一行 ==========
                if ui
                    .add(egui::Button::new("英伟达显卡驱动卸载").min_size(button_size))
                    .clicked()
                {
                    self.show_nvidia_uninstall_dialog = true;
                    self.nvidia_uninstall_message.clear();
                    self.nvidia_uninstall_hardware_summary = None;
                    self.start_load_nvidia_hardware_summary();
                }

                if ui
                    .add(egui::Button::new("分区对拷").min_size(button_size))
                    .clicked()
                {
                    self.show_partition_copy_dialog = true;
                    self.partition_copy_message.clear();
                    self.partition_copy_log.clear();
                    self.partition_copy_source = None;
                    self.partition_copy_target = None;
                    self.start_load_copyable_partitions();
                }

                if ui
                    .add(egui::Button::new("批量格式化").min_size(button_size))
                    .clicked()
                {
                    self.show_batch_format_dialog = true;
                    self.batch_format_message.clear();
                    self.batch_format_partitions.clear();
                    self.batch_format_selected.clear();
                    self.start_load_formatable_partitions();
                }

                if ui
                    .add(egui::Button::new("导入存储驱动").min_size(button_size))
                    .clicked()
                {
                    self.show_import_storage_driver_dialog = true;
                    self.import_storage_driver_message.clear();
                }

                ui.end_row();

                // ========== 第二行 ==========
                if ui
                    .add(egui::Button::new("一键分区").min_size(button_size))
                    .clicked()
                {
                    self.init_quick_partition_dialog();
                }

                if ui
                    .add(egui::Button::new("移除APPX应用").min_size(button_size))
                    .clicked()
                {
                    self.show_remove_appx_dialog = true;
                    self.remove_appx_message.clear();
                    self.remove_appx_list.clear();
                    self.remove_appx_selected.clear();
                }

                if ui
                    .add(egui::Button::new("驱动备份还原").min_size(button_size))
                    .clicked()
                {
                    self.show_driver_backup_dialog = true;
                    self.driver_backup_message.clear();
                }

                if is_pe {
                    if ui
                        .add(egui::Button::new("一键修复引导").min_size(button_size))
                        .clicked()
                    {
                        // 打开一键修复引导对话框，让用户选择分区
                        self.show_repair_boot_dialog = true;
                        self.repair_boot_message.clear();
                        self.repair_boot_selected_partition = None;
                        // 确保Windows分区信息已加载
                        if self.windows_partitions_cache.is_none() && !self.windows_partitions_loading {
                            self.start_load_windows_partitions();
                        }
                    }
                } else {
                    ui.add_enabled(
                        false,
                        egui::Button::new("一键修复引导").min_size(button_size),
                    );
                }

                ui.end_row();

                // ========== 第三行 ==========

                if ui
                    .add(egui::Button::new("本机网络信息").min_size(button_size))
                    .clicked()
                {
                    self.init_network_info_dialog();
                }

                if !is_pe {
                    if ui
                        .add(egui::Button::new("软件列表").min_size(button_size))
                        .clicked()
                    {
                        self.init_software_list_dialog();
                    }
                } else {
                    ui.add_enabled(
                        false,
                        egui::Button::new("软件列表").min_size(button_size),
                    );
                }

                if ui
                    .add(egui::Button::new("系统时间校准").min_size(button_size))
                    .clicked()
                {
                    self.show_time_sync_dialog = true;
                    self.time_sync_message.clear();
                }

                if ui
                    .add(egui::Button::new("手动运行Ghost").min_size(button_size))
                    .clicked()
                {
                    self.launch_ghost_tool();
                }

                ui.end_row();

                // ========== 第四行 ==========

                if ui
                    .add(egui::Button::new("万能驱动").min_size(button_size))
                    .clicked()
                {
                    self.launch_wandrv_tool();
                }

                if ui
                    .add(egui::Button::new("查看GHO密码").min_size(button_size))
                    .clicked()
                {
                    self.show_gho_password_dialog = true;
                    self.gho_password_file_path.clear();
                    self.gho_password_result = None;
                }

                if !is_pe {
                    if ui
                        .add(egui::Button::new("重置网络设置").min_size(button_size))
                        .clicked()
                    {
                        self.show_reset_network_confirm_dialog = true;
                    }
                } else {
                    ui.add_enabled(
                        false,
                        egui::Button::new("重置网络设置").min_size(button_size),
                    );
                }

                if ui
                    .add(egui::Button::new("SpaceSniffer").min_size(button_size))
                    .clicked()
                {
                    self.launch_space_sniffer_tool();
                }

                ui.end_row();

                // ========== 第五行 ==========

                if ui
                    .add(egui::Button::new("镜像校验").min_size(button_size))
                    .clicked()
                {
                    self.show_image_verify_dialog = true;
                    self.image_verify_file_path.clear();
                    self.image_verify_result = None;
                    self.image_verify_progress = None;
                }

                ui.end_row();
            });

        // ========== 对话框渲染 ==========
        self.render_network_info_dialog(ui);
        self.render_import_storage_driver_dialog(ui);
        self.render_remove_appx_dialog(ui);
        self.render_driver_backup_dialog(ui);
        self.render_software_list_dialog(ui);
        self.render_reset_network_confirm_dialog(ui);
        self.render_time_sync_dialog(ui);
        self.render_batch_format_dialog(ui);
        self.render_gho_password_dialog(ui);
        self.render_nvidia_uninstall_dialog(ui);
        self.render_partition_copy_dialog(ui);
        self.render_quick_partition_dialog(ui);
        self.render_image_verify_dialog(ui);
        self.render_repair_boot_dialog(ui);

        // 显示工具状态
        if !self.tool_message.is_empty() {
            ui.add_space(15.0);
            ui.separator();
            ui.label(&self.tool_message);
        }
    }

    /// 启动Ghost工具
    fn launch_ghost_tool(&mut self) {
        match actions::launch_ghost() {
            Ok(_) => {
                self.tool_message = "已启动: Ghost64.exe".to_string();
            }
            Err(e) => {
                self.tool_message = e;
            }
        }
    }

    /// 启动万能驱动工具
    fn launch_wandrv_tool(&mut self) {
        match actions::launch_wandrv() {
            Ok(_) => {
                self.tool_message = "已启动: QDZC.exe".to_string();
            }
            Err(e) => {
                self.tool_message = e;
            }
        }
    }

    /// 启动 SpaceSniffer 磁盘空间分析工具
    fn launch_space_sniffer_tool(&mut self) {
        match actions::launch_space_sniffer() {
            Ok(_) => {
                self.tool_message = "已启动: SpaceSniffer.exe".to_string();
            }
            Err(e) => {
                self.tool_message = e;
            }
        }
    }

    /// 修复引导操作（从对话框调用）
    pub fn repair_boot_action(&mut self) {
        // 从对话框中选择的分区获取
        let target_partition = match &self.repair_boot_selected_partition {
            Some(p) => p.clone(),
            None => {
                self.repair_boot_message = "请先选择目标系统分区".to_string();
                return;
            }
        };

        self.repair_boot_loading = true;
        self.repair_boot_message = "正在修复引导...".to_string();

        match actions::repair_boot(&target_partition) {
            Ok(_) => {
                self.repair_boot_message = format!("✓ 引导修复成功: {}", target_partition);
                self.repair_boot_loading = false;
            }
            Err(e) => {
                self.repair_boot_message = format!("✗ 引导修复失败: {}", e);
                self.repair_boot_loading = false;
            }
        }
    }

    /// 导出驱动操作
    fn export_drivers_action(&mut self, is_pe: bool) {
        let export_dir = crate::utils::path::get_exe_dir()
            .join("drivers_backup")
            .to_string_lossy()
            .to_string();

        self.tool_message = "正在导出驱动...".to_string();

        if is_pe {
            let source_partition = match &self.tool_target_partition {
                Some(p) => p.clone(),
                None => {
                    self.tool_message = "请先选择源系统分区".to_string();
                    return;
                }
            };

            match actions::export_drivers_from_partition(&source_partition, &export_dir) {
                Ok(_) => {
                    self.tool_message =
                        format!("驱动导出成功: {} -> {}", source_partition, export_dir);
                }
                Err(e) => {
                    self.tool_message = format!("驱动导出失败: {}", e);
                }
            }
        } else {
            match actions::export_drivers(&export_dir) {
                Ok(_) => {
                    self.tool_message = format!("驱动导出成功: {}", export_dir);
                }
                Err(e) => {
                    self.tool_message = format!("驱动导出失败: {}", e);
                }
            }
        }
    }

    /// 启动工具
    #[allow(dead_code)]
    fn launch_tool(&mut self, tool_name: &str) {
        match actions::launch_tool(tool_name) {
            Ok(_) => {
                self.tool_message = format!("已启动: {}", tool_name);
            }
            Err(e) => {
                self.tool_message = e;
            }
        }
    }
}