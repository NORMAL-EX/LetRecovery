use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use crate::app::WorkerMessage;
use crate::core::bcdedit::BootManager;
use crate::core::config::{BackupFormat, ConfigFileManager};
use crate::core::dism::{Dism, DismProgress};
use crate::core::ghost::Ghost;
use crate::tr;
use crate::ui::progress::BackupStep;
use crate::utils::reboot_pe;

pub(crate) fn execute_backup_workflow(tx: Sender<WorkerMessage>) {
    log::info!("========== 开始PE备份流程 ==========");

    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(partition) => partition,
        None => {
            let _ = tx.send(WorkerMessage::Failed(tr!("未找到备份配置文件")));
            return;
        }
    };

    log::info!("数据分区: {}", data_partition);
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::ReadConfig));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在读取备份配置...")));

    let config = match ConfigFileManager::read_backup_config(&data_partition) {
        Ok(config) => config,
        Err(error) => {
            let _ = tx.send(WorkerMessage::Failed(tr!("读取配置失败: {}", error)));
            return;
        }
    };

    lr_core::set_active_engine(lr_core::WimEngine::from_u8(config.wim_engine));

    log::info!("源分区: {}", config.source_partition);
    log::info!("保存路径: {}", config.save_path);
    log::info!("备份格式: {:?}", config.format);
    if config.format == BackupFormat::Swm {
        log::info!("SWM分卷大小: {} MB", config.swm_split_size);
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let source_partition = ConfigFileManager::find_backup_marker_partition()
        .unwrap_or_else(|| config.source_partition.clone());
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::CaptureImage));

    let capture_dir = format!("{}\\", source_partition);
    let (progress_tx, progress_rx) = channel::<DismProgress>();
    let progress_message_tx = tx.clone();
    let progress_handle = thread::spawn(move || {
        while let Ok(progress) = progress_rx.recv() {
            let _ = progress_message_tx.send(WorkerMessage::SetProgress(progress.percentage));
        }
    });

    let backup_result = match config.format {
        BackupFormat::Gho => {
            let _ = tx.send(WorkerMessage::SetStatus(tr!("正在使用Ghost备份系统...")));
            let ghost = Ghost::new();
            if !ghost.is_available() {
                drop(progress_handle);
                let _ = tx.send(WorkerMessage::Failed(tr!("Ghost工具不可用")));
                return;
            }
            ghost.create_image_from_letter(&source_partition, &config.save_path, Some(progress_tx))
        }
        BackupFormat::Esd => {
            let _ = tx.send(WorkerMessage::SetStatus(tr!(
                "正在备份系统（ESD高压缩）..."
            )));
            let dism = Dism::new();
            if config.incremental && Path::new(&config.save_path).exists() {
                dism.append_image_esd(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            } else {
                dism.capture_image_esd(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            }
        }
        BackupFormat::Swm => {
            let _ = tx.send(WorkerMessage::SetStatus(tr!(
                "正在备份系统（SWM分卷，每卷{}MB）...",
                config.swm_split_size
            )));
            Dism::new().capture_image_swm(
                &config.save_path,
                &capture_dir,
                &config.name,
                &config.description,
                config.swm_split_size,
                Some(progress_tx),
            )
        }
        BackupFormat::Wim => {
            let _ = tx.send(WorkerMessage::SetStatus(tr!("正在执行系统备份...")));
            let dism = Dism::new();
            if config.incremental && Path::new(&config.save_path).exists() {
                dism.append_image(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            } else {
                dism.capture_image(
                    &config.save_path,
                    &capture_dir,
                    &config.name,
                    &config.description,
                    Some(progress_tx),
                )
            }
        }
    };

    let _ = progress_handle.join();
    if let Err(error) = backup_result {
        let _ = tx.send(WorkerMessage::Failed(tr!("备份失败: {}", error)));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::VerifyBackup));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在验证备份文件...")));
    if !Path::new(&config.save_path).exists() {
        let _ = tx.send(WorkerMessage::Failed(tr!("备份文件验证失败")));
        return;
    }
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::RepairBoot));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在恢复引导...")));
    let _ = BootManager::new().delete_current_boot_entry();
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::Cleanup));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理临时文件...")));
    ConfigFileManager::cleanup_partition_markers(&source_partition);
    ConfigFileManager::cleanup_data_dir(&data_partition);
    ConfigFileManager::cleanup_pe_dir(&data_partition);
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::Complete));
    let _ = tx.send(WorkerMessage::Completed);
    log::info!("========== PE备份流程完成 ==========");

    log::info!("即将重启...");
    thread::sleep(Duration::from_secs(3));
    reboot_pe();
}
