//! 目标分区容量规划：镜像展开后能不能装进目标分区；装不下时，能不能用 WOF 紧凑文件
//! （“压缩释放”，与 Windows 的 CompactOS 相同的机制）装下。
//!
//! 只依据 WIM 元数据（TOTALBYTES − HARDLINKBYTES）和镜像文件大小，在格式化或删除旧系统之前
//! 做出决定：确定装不下就停止，旧系统原样保留，而不是格式化之后才发现“磁盘空间不足”。
//! 紧凑文件只能用于 Windows 10 及以上的镜像（更早的系统读不了这种文件）。

/// 新系统首次启动的页面文件、休眠文件和更新所需的余量。
pub const ALLOWANCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// 能读取 WOF 紧凑文件的最早 Windows 版本（Windows 10 1507）。
pub const MIN_COMPACT_BUILD: u32 = 10240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPlan {
    /// 镜像元数据不完整，按普通方式释放（失败时仍可按紧凑方式重试）。
    Unknown,
    Normal,
    CompactXpress4k,
    CompactLzx,
    TooSmall { required_bytes: u64 },
}

/// 格式化后的可用空间：扣除 NTFS 自身元数据（约 1.5%）和 256 MiB。
pub fn usable_capacity(partition_bytes: u64) -> u64 {
    partition_bytes
        .saturating_sub(partition_bytes / 66)
        .saturating_sub(256 * 1024 * 1024)
}

/// LZX 紧凑文件的压缩率与 WIM 内部的 LZX 压缩接近，用镜像文件大小估算；ESD（LZMS 固实压缩）
/// 比 LZX 紧凑文件压得更狠，余量放大。结果不低于展开大小的 55%，也不超过展开大小。
pub fn lzx_estimate(expanded_bytes: u64, image_file_bytes: u64, solid_image: bool) -> u64 {
    let estimate = if image_file_bytes == 0 {
        expanded_bytes / 100 * 65
    } else if solid_image {
        image_file_bytes.saturating_add(image_file_bytes / 100 * 45)
    } else {
        image_file_bytes.saturating_add(image_file_bytes / 100 * 15)
    };
    estimate.max(expanded_bytes / 100 * 55).min(expanded_bytes)
}

/// XPRESS4K 紧凑文件对系统和程序文件通常能省 25%–40%，按 72% 估算。
pub fn xpress_estimate(expanded_bytes: u64) -> u64 {
    expanded_bytes / 100 * 72
}

pub fn plan(
    expanded_bytes: u64,
    image_file_bytes: u64,
    solid_image: bool,
    partition_bytes: u64,
    compact_supported: bool,
) -> ApplyPlan {
    if expanded_bytes == 0 || partition_bytes == 0 {
        return ApplyPlan::Unknown;
    }
    let usable = usable_capacity(partition_bytes);
    let need = |bytes: u64| bytes.saturating_add(ALLOWANCE_BYTES);
    if need(expanded_bytes) <= usable {
        return ApplyPlan::Normal;
    }
    if !compact_supported {
        return ApplyPlan::TooSmall {
            required_bytes: need(expanded_bytes),
        };
    }
    if need(xpress_estimate(expanded_bytes)) <= usable {
        return ApplyPlan::CompactXpress4k;
    }
    let lzx = lzx_estimate(expanded_bytes, image_file_bytes, solid_image);
    if need(lzx) <= usable {
        return ApplyPlan::CompactLzx;
    }
    ApplyPlan::TooSmall {
        required_bytes: need(lzx),
    }
}

pub fn gib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn chooses_the_least_compression_that_fits() {
        assert_eq!(plan(20 * GIB, 6 * GIB, false, 100 * GIB, true), ApplyPlan::Normal);
        // 78 GB 的备份展开 120 GB，目标 110 GB：XPRESS4K 估算 86 GB 可以装下。
        assert_eq!(plan(120 * GIB, 78 * GIB, false, 110 * GIB, true), ApplyPlan::CompactXpress4k);
        // 镜像文件 60 GB、目标 85 GB：XPRESS 估算 86 GB 装不下，LZX 估算约 69 GB 可以。
        assert_eq!(plan(120 * GIB, 60 * GIB, false, 85 * GIB, true), ApplyPlan::CompactLzx);
        assert!(matches!(plan(120 * GIB, 60 * GIB, false, 60 * GIB, true), ApplyPlan::TooSmall { .. }));
        assert!(matches!(plan(120 * GIB, 78 * GIB, false, 110 * GIB, false), ApplyPlan::TooSmall { .. }));
        assert_eq!(plan(0, 0, false, 100 * GIB, true), ApplyPlan::Unknown);
    }
}
