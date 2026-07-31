//! Typed execution boundary for the native lossless C: expansion handoff.
//!
//! The worker re-runs the read-only layout analysis and rejects any changed snapshot before it
//! writes the existing PE configuration or installs a boot entry. It never performs the actual
//! partition move/extend; that remains the established PE workflow.

use std::sync::mpsc::Receiver;

use crate::download::config::OnlinePE;

#[derive(Clone, Debug)]
pub struct ExpandCHandoffRequest {
    pub target_partition: char,
    pub expected_disk: Option<crate::core::native_quick_partition::DiskFingerprint>,
    pub expected_partition_number: Option<u32>,
    pub target_size_mb: u64,
    pub use_maximum: bool,
    pub analyzed_current_size_mb: u64,
    pub analyzed_max_size_mb: u64,
    pub analyzed_no_move_max_mb: u64,
    pub strict_analysis_snapshot: bool,
    pub borrow_from_left: bool,
    pub minimum_free_mb: u64,
    pub wim_engine: u8,
    pub pe: OnlinePE,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandCWorkerMessage {
    Progress(String),
    ReadyToReboot,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ExpandCStartError {
    #[error("开发测试构建禁止准备真实扩容或 PE 启动环境")]
    DisabledInDevelopment,
    #[error("无法启动扩容准备线程: {0}")]
    Spawn(String),
}

#[cfg(feature = "non-elevated-tests")]
pub fn start_expand_c_handoff(
    _request: ExpandCHandoffRequest,
) -> Result<Receiver<ExpandCWorkerMessage>, ExpandCStartError> {
    Err(ExpandCStartError::DisabledInDevelopment)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn start_expand_c_handoff(
    request: ExpandCHandoffRequest,
) -> Result<Receiver<ExpandCWorkerMessage>, ExpandCStartError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("letrecovery-native-expand-c".to_owned())
        .spawn(move || {
            if let Err(error) = run_handoff(&request, &sender) {
                let _ = sender.send(ExpandCWorkerMessage::Failed(error));
            }
        })
        .map_err(|error| ExpandCStartError::Spawn(error.to_string()))?;
    Ok(receiver)
}

#[cfg(not(feature = "non-elevated-tests"))]
fn run_handoff(
    request: &ExpandCHandoffRequest,
    sender: &std::sync::mpsc::Sender<ExpandCWorkerMessage>,
) -> Result<(), String> {
    use crate::core::install_config::{ConfigFileManager, ExpandConfig};
    use lr_core::cached_artifact::CachedArtifactStatus;

    let target_partition = request.target_partition.to_ascii_uppercase();
    let _ = sender.send(ExpandCWorkerMessage::Progress(crate::tr!(
        "正在重新确认分区 {}: 布局...",
        target_partition
    )));
    let fresh = if request.borrow_from_left {
        super::native_expand_c_controller::analyze_expand_partition_from_left(target_partition)
    } else {
        super::native_expand_c_controller::analyze_expand_partition(target_partition)
    }
    .map_err(|error| error.to_string())?;
    let strict_snapshot_changed = request.strict_analysis_snapshot
        && (fresh.max_size_mb != request.analyzed_max_size_mb
            || fresh.no_move_max_mb != request.analyzed_no_move_max_mb);
    let target_identity_changed = request
        .expected_disk
        .as_ref()
        .is_some_and(|expected| fresh.disk.as_ref() != Some(expected))
        || request
            .expected_partition_number
            .is_some_and(|expected| fresh.partition_number != expected);
    if !fresh.found
        || fresh.max_size_mb <= fresh.current_size_mb
        || fresh.current_size_mb != request.analyzed_current_size_mb
        || strict_snapshot_changed
        || target_identity_changed
    {
        return Err(crate::tr!(
            "分区 {}: 布局已变化，请重新分析后再试。",
            target_partition
        ));
    }
    let minimum = fresh
        .current_size_mb
        .max(fresh.used_mb.saturating_add(request.minimum_free_mb));
    if request.target_size_mb < minimum || request.target_size_mb > fresh.max_size_mb {
        return Err(crate::tr!("目标大小已不在当前安全范围内，请重新分析。"));
    }
    let disk = fresh.disk.as_ref().ok_or_else(|| {
        crate::tr!(
            "分区 {}: 缺少执行前磁盘身份，请重新分析。",
            target_partition
        )
    })?;
    let target_fingerprint = disk
        .partitions
        .iter()
        .find(|partition| partition.partition_number == fresh.partition_number)
        .ok_or_else(|| crate::tr!("执行前未能在磁盘指纹中定位目标分区。"))?;
    let donor_fingerprint = if request.borrow_from_left {
        Some(
            disk.partitions
                .iter()
                .find(|partition| {
                    partition.offset_bytes.checked_add(partition.size_bytes)
                        == Some(target_fingerprint.offset_bytes)
                })
                .ok_or_else(|| crate::tr!("执行前未能在磁盘指纹中定位左侧相邻分区。"))?,
        )
    } else {
        None
    };

    let pe_path = match super::pe::PeManager::check_cached_pe(
        &request.pe.filename,
        request.pe.sha256.as_deref(),
        request.pe.md5.as_deref(),
    ) {
        Ok(CachedArtifactStatus::Ready { path, .. }) => path,
        Ok(CachedArtifactStatus::Missing) => {
            return Err(crate::tr!("所选 PE 文件不存在，请重新下载。"));
        }
        Err(error) => return Err(crate::tr!("PE 文件安全校验失败：{}", error)),
    };

    let _ = sender.send(ExpandCWorkerMessage::Progress(crate::tr!(
        "正在写入扩容配置..."
    )));
    let config = ExpandConfig {
        target_partition: format!("{}:", target_partition),
        target_size_mb: if request.use_maximum {
            0
        } else {
            request.target_size_mb
        },
        wim_engine: request.wim_engine,
        borrow_from_left: request.borrow_from_left,
        expected_disk_number: disk.disk_number,
        expected_disk_size_bytes: disk.size_bytes,
        expected_partition_number: target_fingerprint.partition_number,
        expected_partition_offset_bytes: target_fingerprint.offset_bytes,
        expected_partition_size_bytes: target_fingerprint.size_bytes,
        expected_donor_partition_number: donor_fingerprint
            .map_or(0, |partition| partition.partition_number),
        expected_donor_offset_bytes: donor_fingerprint
            .map_or(0, |partition| partition.offset_bytes),
        expected_donor_size_bytes: donor_fingerprint.map_or(0, |partition| partition.size_bytes),
    };
    let target = format!("{}:", target_partition);
    let transaction =
        ConfigFileManager::write_expand_config_transactional(&target, &target, &config)
            .map_err(|error| crate::tr!("写入扩容配置失败: {}", error))?;

    let _ = sender.send(ExpandCWorkerMessage::Progress(crate::tr!(
        "正在安装 PE 启动项"
    )));
    install_pe_boot_with_rollback(
        transaction,
        super::pe::PeManager::new()
            .boot_to_pe(&pe_path.to_string_lossy(), &request.pe.display_name)
            .map_err(|error| error.to_string()),
    )?;
    let _ = sender.send(ExpandCWorkerMessage::ReadyToReboot);
    Ok(())
}

fn install_pe_boot_with_rollback(
    transaction: crate::core::install_config::ExpandConfigTransaction,
    install_result: Result<(), String>,
) -> Result<(), String> {
    match install_result {
        Ok(()) => Ok(()),
        Err(error) => match transaction.rollback() {
            Ok(()) => Err(crate::tr!("安装 PE 引导失败，扩容配置已回滚: {}", error)),
            Err(rollback_error) => Err(crate::tr!(
                "安装 PE 引导失败: {}; 扩容配置回滚也失败: {}",
                error,
                rollback_error
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_root() -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "letrecovery-expand-executor-{}-{nonce}",
            std::process::id()
        ))
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_refuses_before_starting_a_worker() {
        let request = ExpandCHandoffRequest {
            target_partition: 'C',
            expected_disk: None,
            expected_partition_number: None,
            target_size_mb: 1,
            use_maximum: false,
            analyzed_current_size_mb: 1,
            analyzed_max_size_mb: 2,
            analyzed_no_move_max_mb: 2,
            strict_analysis_snapshot: true,
            borrow_from_left: false,
            minimum_free_mb: 1024,
            wim_engine: 0,
            pe: OnlinePE {
                download_url: "https://example.invalid/pe.wim".to_owned(),
                display_name: "Test PE".to_owned(),
                filename: "test.wim".to_owned(),
                md5: None,
                sha256: Some("00".repeat(32)),
            },
        };
        assert!(matches!(
            start_expand_c_handoff(request),
            Err(ExpandCStartError::DisabledInDevelopment)
        ));
    }

    #[test]
    fn pe_boot_failure_rolls_back_only_the_expand_transaction() {
        use crate::core::install_config::{ConfigFileManager, ExpandConfig};

        let root = unique_temp_root();
        let data_dir = root.join("LetRecovery_Data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let marker = root.join("LetRecovery_Expand.marker");
        let config = data_dir.join("LetRecovery_Expand.ini");
        let unrelated = data_dir.join("user-owned.bin");
        std::fs::write(&marker, b"previous marker").unwrap();
        std::fs::write(&config, b"previous config").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();
        let partition = root.to_string_lossy();
        let transaction = ConfigFileManager::write_expand_config_transactional(
            &partition,
            &partition,
            &ExpandConfig {
                target_partition: "C:".to_owned(),
                target_size_mb: 0,
                wim_engine: 0,
                borrow_from_left: false,
                expected_disk_number: 0,
                expected_disk_size_bytes: 0,
                expected_partition_number: 0,
                expected_partition_offset_bytes: 0,
                expected_partition_size_bytes: 0,
                expected_donor_partition_number: 0,
                expected_donor_offset_bytes: 0,
                expected_donor_size_bytes: 0,
            },
        )
        .unwrap();

        let error =
            install_pe_boot_with_rollback(transaction, Err("simulated BCD failure".to_owned()))
                .unwrap_err();
        assert!(error.contains("simulated BCD failure"));
        assert_eq!(std::fs::read(&marker).unwrap(), b"previous marker");
        assert_eq!(std::fs::read(&config).unwrap(), b"previous config");
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
        std::fs::remove_dir_all(root).unwrap();
    }
}
