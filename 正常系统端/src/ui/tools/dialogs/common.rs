use egui;
use std::sync::mpsc;
use crate::tr;
use crate::app::App;
use super::super::types::WindowsPartitionInfo;
use super::super::version_detect::get_windows_partition_infos;

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
                    self.remove_appx_message = tr!("未找到可移除的应用");
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
                self.remove_appx_message = tr!("移除完成: 成功 {}, 失败 {}", success, fail);
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
                    self.time_sync_message = tr!(
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
                let mut msg = tr!(
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
}

/// 格式化分区显示文本
pub(super) fn format_partition_display(partitions: &[WindowsPartitionInfo], letter: &str) -> String {
    partitions
        .iter()
        .find(|p| p.letter == letter)
        .map(|p| format!("{} [{}] [{}]", p.letter, p.windows_version, p.architecture))
        .unwrap_or_else(|| letter.to_string())
}

/// 根据消息内容获取颜色
pub(super) fn get_message_color(message: &str) -> egui::Color32 {
    if message.contains("成功") {
        egui::Color32::from_rgb(0, 180, 0)
    } else if message.contains("失败") || message.contains("错误") || message.contains("不存在") {
        egui::Color32::from_rgb(255, 80, 80)
    } else {
        egui::Color32::GRAY
    }
}
