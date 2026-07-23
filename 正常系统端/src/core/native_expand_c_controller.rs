//! Read-only analysis boundary for the native "lossless expand C:" dialog.
//!
//! This preserves the legacy capacity calculation, including the distinction between adjacent
//! unallocated space (pure extend) and space which requires moving the following data partition.
//! It never writes a partition table, shrinks or moves a volume, prepares PE, or restarts Windows.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeExpandCAnalysis {
    pub found: bool,
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

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_c() -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    use crate::core::quick_partition::{get_physical_disks, query_shrink_max};

    const BYTES_PER_MB: u64 = 1024 * 1024;
    let disks = get_physical_disks();
    let Some((disk, c_index)) = disks.iter().find_map(|disk| {
        disk.partitions
            .iter()
            .position(|partition| partition.drive_letter == Some('C'))
            .map(|index| (disk, index))
    }) else {
        return Ok(NativeExpandCAnalysis {
            reason: crate::tr!("未找到当前系统 C 盘"),
            ..Default::default()
        });
    };

    let c_partition = &disk.partitions[c_index];
    let current_size_mb = c_partition.size_bytes / BYTES_PER_MB;
    let c_end = c_partition
        .offset_bytes
        .saturating_add(c_partition.size_bytes);
    let mut following: Vec<_> = disk
        .partitions
        .iter()
        .filter(|partition| partition.offset_bytes >= c_end)
        .collect();
    following.sort_by_key(|partition| partition.offset_bytes);

    let unallocated_after_bytes = following.first().map_or_else(
        || disk.size_bytes.saturating_sub(c_end),
        |next| next.offset_bytes.saturating_sub(c_end),
    );
    let unallocated_after_mb = unallocated_after_bytes / BYTES_PER_MB;
    let mut next_shrinkable_mb = 0;
    if let Some(next) = following.first() {
        let adjacent = next.offset_bytes.saturating_sub(c_end) < 2 * BYTES_PER_MB;
        let movable = adjacent && !next.is_esp && !next.is_msr && !next.is_recovery;
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
        crate::tr!("C 盘后方没有可用于扩容的空间。可先用「一键分区」在 C 盘后方腾出未分配空间。")
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
        current_size_mb,
        used_mb: c_partition.used_bytes / BYTES_PER_MB,
        free_mb: c_partition.free_bytes / BYTES_PER_MB,
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
