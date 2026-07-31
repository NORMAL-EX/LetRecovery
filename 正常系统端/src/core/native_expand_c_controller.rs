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

#[cfg(feature = "non-elevated-tests")]
pub fn analyze_expand_partition_from_left(
    _target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    Err(NativeExpandCAnalysisError::DisabledInDevelopment)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_c() -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    analyze_expand_partition('C')
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_partition(
    target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    use crate::core::quick_partition::{get_physical_disks, query_shrink_max};

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
    let mut next_shrinkable_mb = 0;
    if let Some(next) = following.first() {
        let system_letter = std::env::var("SystemDrive")
            .ok()
            .and_then(|value| value.chars().next())
            .map(|letter| letter.to_ascii_uppercase());
        let movable = !next.is_esp
            && !next.is_msr
            && !next.is_recovery
            && next
                .drive_letter
                .is_some_and(|letter| Some(letter.to_ascii_uppercase()) != system_letter);
        if movable {
            if let Some(letter) = next.drive_letter {
                if let Ok(value) = query_shrink_max(letter) {
                    next_shrinkable_mb = value;
                }
            }
        }
    }

    let no_move_max_mb = current_size_mb.saturating_add(unallocated_after_mb);
    let max_size_mb = no_move_max_mb.saturating_add(next_shrinkable_mb);
    let can_expand = max_size_mb > current_size_mb.saturating_add(1024);
    let reason = if !can_expand {
        crate::tr!("分区 {}: 后方没有可用于扩容的空间。", target_letter)
    } else if next_shrinkable_mb > 1024 {
        crate::tr!(
            "可无损并入：相邻未分配约 {} GB（直接扩）+ 后方分区可让出约 {} GB（需移动该分区的数据）。",
            format!("{:.1}", unallocated_after_mb as f64 / 1024.0),
            format!("{:.1}", next_shrinkable_mb as f64 / 1024.0)
        )
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

/// Computes how far a target volume can grow when its immediately preceding basic data volume
/// yields space. The actual move remains an offline PE operation.
#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_partition_from_left(
    target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    use crate::core::quick_partition::{get_physical_disks, query_shrink_max};

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

    let target = &disk.partitions[target_index];
    let current_size_mb = target.size_bytes / BYTES_PER_MB;
    let previous = disk.partitions.iter().find(|partition| {
        partition.offset_bytes.checked_add(partition.size_bytes) == Some(target.offset_bytes)
    });
    let system_letter = std::env::var("SystemDrive")
        .ok()
        .and_then(|value| value.chars().next())
        .map(|letter| letter.to_ascii_uppercase());
    let target_movable = !target.is_esp
        && !target.is_msr
        && !target.is_recovery
        && target.file_system.trim().eq_ignore_ascii_case("NTFS")
        && target
            .drive_letter
            .is_some_and(|letter| Some(letter.to_ascii_uppercase()) != system_letter);
    let previous_movable = previous.is_some_and(|partition| {
        !partition.is_esp
            && !partition.is_msr
            && !partition.is_recovery
            && partition.file_system.trim().eq_ignore_ascii_case("NTFS")
            && partition
                .drive_letter
                .is_some_and(|letter| Some(letter.to_ascii_uppercase()) != system_letter)
    });
    let previous_shrinkable_mb = if target_movable && previous_movable {
        previous
            .and_then(|partition| partition.drive_letter)
            .and_then(|letter| query_shrink_max(letter).ok())
            .unwrap_or(0)
    } else {
        0
    };
    let max_size_mb = current_size_mb.saturating_add(previous_shrinkable_mb);
    let can_expand = max_size_mb > current_size_mb;
    let reason = if can_expand {
        String::new()
    } else {
        crate::tr!(
            "分区 {}: 左侧没有可安全用于扩容的相邻数据分区空间。",
            target_letter
        )
    };
    Ok(NativeExpandCAnalysis {
        found: true,
        disk: Some(crate::core::native_quick_partition::DiskFingerprint::from(
            disk,
        )),
        partition_number: target.partition_number,
        current_size_mb,
        used_mb: target.used_bytes / BYTES_PER_MB,
        free_mb: target.free_bytes / BYTES_PER_MB,
        max_size_mb,
        // Growing at the beginning always requires relocating the target partition.
        no_move_max_mb: current_size_mb,
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
