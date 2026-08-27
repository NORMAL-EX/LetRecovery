//! Read-only analysis boundary for the native "lossless expand C:" dialog.
//!
//! This preserves the legacy capacity calculation, including the distinction between adjacent
//! unallocated space (pure extend) and space which requires moving the following data partition.
//! It never writes a partition table, shrinks or moves a volume, prepares PE, or restarts Windows.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeExpandCAnalysis {
    pub found: bool,
    pub disk: Option<crate::core::native_quick_partition::DiskFingerprint>,
    pub partition_number: u32,
    pub current_size_mb: u64,
    pub used_mb: u64,
    pub free_mb: u64,
    pub max_size_mb: u64,
    pub no_move_max_mb: u64,
    pub can_expand: bool,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NativeExpandCAnalysisError {
    #[error("开发测试构建禁止读取宿主磁盘扩容布局")]
    DisabledInDevelopment,
    #[error("无法确定当前运行的 Windows 卷: {0}")]
    CurrentWindowsVolume(String),
}

/// Reads the current disk inventory and computes the established expand-C safety limits.
/// The returned snapshot is advisory: the eventual PE handoff must enumerate again and compare a
/// stable disk/partition fingerprint before writing anything.
#[cfg(feature = "non-elevated-tests")]
pub fn analyze_expand_c() -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    Err(NativeExpandCAnalysisError::DisabledInDevelopment)
}

#[cfg(feature = "non-elevated-tests")]
pub fn analyze_expand_partition(
    _target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    Err(NativeExpandCAnalysisError::DisabledInDevelopment)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_c() -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    let drive = lr_core::windows_storage::current_windows_drive_letter()
        .map_err(|error| NativeExpandCAnalysisError::CurrentWindowsVolume(error.to_string()))?;
    analyze_expand_partition(drive)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_partition(
    target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    use crate::core::quick_partition::get_physical_disks;

    const BYTES_PER_MB: u64 = 1024 * 1024;
    let target_letter = target_letter.to_ascii_uppercase();
    let disks = get_physical_disks();
    let Some((disk, target_index)) = disks.iter().find_map(|disk| {
        disk.partitions
            .iter()
            .position(|partition| {
                partition
                    .drive_letter
                    .is_some_and(|letter| letter.eq_ignore_ascii_case(&target_letter))
            })
            .map(|index| (disk, index))
    }) else {
        return Ok(NativeExpandCAnalysis {
            reason: crate::tr!("未找到目标分区 {}:", target_letter),
            ..Default::default()
        });
    };

    let target_partition = &disk.partitions[target_index];
    let current_size_mb = target_partition.size_bytes / BYTES_PER_MB;
    let target_end = target_partition
        .offset_bytes
        .saturating_add(target_partition.size_bytes);
    let mut following: Vec<_> = disk
        .partitions
        .iter()
        .filter(|partition| partition.offset_bytes >= target_end)
        .collect();
    following.sort_by_key(|partition| partition.offset_bytes);

    let unallocated_after_bytes = following.first().map_or_else(
        || disk.size_bytes.saturating_sub(target_end),
        |next| next.offset_bytes.saturating_sub(target_end),
    );
    let unallocated_after_mb = unallocated_after_bytes / BYTES_PER_MB;
    let no_move_max_mb = current_size_mb.saturating_add(unallocated_after_mb);
    // The production LRPE4 workflow currently permits only the already-present contiguous free
    // extent. QueryMaxReclaimableBytes is an estimate and raw partition movement is fail-closed,
    // so neither may inflate the executable UI limit.
    let max_size_mb = no_move_max_mb;
    let can_expand = max_size_mb > current_size_mb.saturating_add(1024);
    let reason = if !can_expand {
        crate::tr!("分区 {}: 后方没有可用于扩容的空间。", target_letter)
    } else {
        String::new()
    };

    Ok(NativeExpandCAnalysis {
        found: true,
        disk: Some(crate::core::native_quick_partition::DiskFingerprint::from(
            disk,
        )),
        partition_number: target_partition.partition_number,
        current_size_mb,
        used_mb: target_partition.used_bytes / BYTES_PER_MB,
        free_mb: target_partition.free_bytes / BYTES_PER_MB,
        max_size_mb,
        no_move_max_mb,
        can_expand,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_analysis_is_fail_closed() {
        let analysis = NativeExpandCAnalysis::default();
        assert!(!analysis.found);
        assert!(!analysis.can_expand);
        assert_eq!(analysis.max_size_mb, 0);
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_refuses_host_disk_inventory() {
        assert_eq!(
            analyze_expand_c(),
            Err(NativeExpandCAnalysisError::DisabledInDevelopment)
        );
    }
}
