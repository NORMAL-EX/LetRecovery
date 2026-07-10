use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::app::WorkerMessage;
use crate::core::bcdedit::BootManager;
use crate::core::config::ConfigFileManager;
use crate::tr;
use crate::utils::reboot_pe;

/// Execute the non-destructive system-partition expansion workflow.
pub(crate) fn execute_expand_workflow(tx: Sender<WorkerMessage>) {
    log::info!("========== 开始PE扩容流程 ==========");

    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(partition) => partition,
        None => {
            let _ = tx.send(WorkerMessage::Failed(tr!("未找到扩容配置文件")));
            return;
        }
    };
    let config = match ConfigFileManager::read_expand_config(&data_partition) {
        Ok(config) => config,
        Err(error) => {
            let _ = tx.send(WorkerMessage::Failed(tr!("读取扩容配置失败: {}", error)));
            return;
        }
    };

    // PE drive letters are not stable, so prefer the marker over the letter
    // recorded by the desktop endpoint.
    let target_partition = ConfigFileManager::find_expand_marker_partition()
        .unwrap_or_else(|| config.target_partition.clone());
    let letter = target_partition
        .trim_end_matches(':')
        .chars()
        .next()
        .unwrap_or('C');

    let _ = tx.send(WorkerMessage::SetStatus(tr!(
        "正在无损扩大分区 {}: （目标 {} MB，0=最大）...",
        letter,
        config.target_size_mb
    )));
    let _ = tx.send(WorkerMessage::SetProgress(30));
    log::info!(
        "[EXPAND] 目标分区: {}:，目标大小: {} MB",
        letter,
        config.target_size_mb
    );

    match crate::core::expand_move::expand_c_drive(letter, config.target_size_mb, &data_partition) {
        Ok(message) => {
            log::info!("[EXPAND] {}", message);
            let _ = tx.send(WorkerMessage::SetStatus(message));
            let _ = tx.send(WorkerMessage::SetProgress(90));
        }
        Err(error) => {
            log::error!("[EXPAND] 扩容失败: {}", error);
            let _ = tx.send(WorkerMessage::Failed(tr!("扩容失败: {}", error)));
            cleanup_after_expand(&target_partition, &data_partition, false);
            std::thread::sleep(Duration::from_secs(5));
            reboot_pe();
            return;
        }
    }

    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理临时文件...")));
    cleanup_after_expand(&target_partition, &data_partition, true);

    let _ = tx.send(WorkerMessage::SetProgress(100));
    let _ = tx.send(WorkerMessage::Completed);
    log::info!("========== PE扩容流程完成 ==========");

    log::info!("即将重启...");
    std::thread::sleep(Duration::from_secs(3));
    reboot_pe();
}

fn cleanup_after_expand(target_partition: &str, data_partition: &str, log_boot_error: bool) {
    ConfigFileManager::cleanup_partition_markers(target_partition);
    ConfigFileManager::cleanup_data_dir(data_partition);
    let boot_manager = BootManager::new();
    if let Err(error) = boot_manager.delete_current_boot_entry() {
        if log_boot_error {
            log::warn!("[EXPAND] 删除 PE 引导项失败（不影响结果）: {}", error);
        }
    }
    ConfigFileManager::cleanup_pe_dir(data_partition);
}
