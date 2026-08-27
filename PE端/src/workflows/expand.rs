use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::app::WorkerMessage;
use crate::core::config::AuthenticatedOperationConfig;
use crate::tr;
use crate::utils::reboot_pe;

/// Execute the non-destructive system-partition expansion workflow.
pub(crate) fn execute_expand_workflow(
    tx: Sender<WorkerMessage>,
    authenticated_handoff: crate::core::config::AuthenticatedOperationGuard,
) {
    log::info!("========== 开始PE扩容流程 ==========");
    let authenticated_task = match authenticated_handoff.into_task() {
        Ok(task) => task,
        Err(error) => {
            let _ = tx.send(WorkerMessage::Failed(tr!("扩容任务认证失效: {}", error)));
            return;
        }
    };
    let config = match authenticated_task.config() {
        AuthenticatedOperationConfig::Expand(config) => config.clone(),
        _ => {
            let _ = tx.send(WorkerMessage::Failed(tr!("认证任务不是扩容操作")));
            return;
        }
    };
    let data_partition = authenticated_task
        .data_volume_root()
        .to_string_lossy()
        .into_owned();
    let expected_target = authenticated_task.data_volume_identity();
    let Some(letter) = authenticated_task
        .data_partition()
        .chars()
        .next()
        .filter(char::is_ascii_alphabetic)
        .map(|letter| letter.to_ascii_uppercase())
    else {
        let _ = tx.send(WorkerMessage::Failed(
            "authenticated expansion target has no valid drive letter".to_owned(),
        ));
        return;
    };
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

    let expand_result = if config.borrow_from_left {
        crate::core::expand_move::expand_from_left_donor(
            letter,
            &config,
            &data_partition,
            expected_target,
            false,
        )
    } else {
        crate::core::expand_move::expand_c_drive(letter, &config, &data_partition, expected_target)
    };
    match expand_result {
        Ok(message) => {
            log::info!("[EXPAND] {}", message);
            let _ = tx.send(WorkerMessage::SetStatus(message));
            let _ = tx.send(WorkerMessage::SetProgress(90));
        }
        Err(error) => {
            log::error!("[EXPAND] 扩容失败: {}", error);
            let _ = tx.send(WorkerMessage::Failed(tr!("扩容失败: {}", error)));
            log::warn!(
                "[EXPAND] preserving task files and PE boot state for diagnosis and an explicit retry"
            );
            return;
        }
    }

    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理临时文件...")));
    if let Err(error) = authenticated_task.verify_unchanged() {
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "扩容已经完成，但认证任务在清理前发生变化: {}",
            error
        )));
        return;
    }
    if let Err(error) = crate::cleanup_persistent_pe_boot_payload(authenticated_task.guard()) {
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "扩容已经完成，但清理本次 PE 启动项失败: {}",
            error
        )));
        return;
    }
    if let Err(error) = authenticated_task.cleanup_public_control_files() {
        let _ = tx.send(WorkerMessage::Failed(tr!(
            "扩容已完成，但删除认证会话文件失败: {}",
            error
        )));
        return;
    }

    let _ = tx.send(WorkerMessage::SetProgress(100));
    let _ = tx.send(WorkerMessage::Completed);
    log::info!("========== PE扩容流程完成 ==========");

    log::info!("即将重启...");
    std::thread::sleep(Duration::from_secs(3));
    reboot_pe();
}
