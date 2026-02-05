//! BitLocker解锁UI模块
//!
//! 提供BitLocker加密分区的检测和解锁功能的UI接口
//! 实际功能由 core::bitlocker 模块实现

// 重新导出核心模块的类型，保持API兼容
pub use crate::core::bitlocker::{
    VolumeInfo as BitLockerPartition,
    UnlockResult,
};

// 重新导出便捷函数，保持API兼容
pub use crate::core::bitlocker::{
    get_encrypted_partitions as get_bitlocker_partitions,
    get_locked_partitions as get_locked_bitlocker_partitions,
    has_locked_partitions as has_locked_bitlocker_partitions,
    has_locked_partitions as has_bitlocker_partitions,
    unlock_partition_with_password as unlock_with_password,
    unlock_partition_with_recovery_key as unlock_with_recovery_key,
    partition_needs_unlock,
    decrypt_partition,
    partition_can_decrypt,
};

/// 为兼容性提供的别名
pub type BitLockerPartitionInfo = BitLockerPartition;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_compatibility() {
        // 确保API兼容性
        let _: Vec<BitLockerPartition> = get_bitlocker_partitions();
        let _: Vec<BitLockerPartition> = get_locked_bitlocker_partitions();
        let _: bool = has_bitlocker_partitions();
        let _: bool = has_locked_bitlocker_partitions();
    }
}
