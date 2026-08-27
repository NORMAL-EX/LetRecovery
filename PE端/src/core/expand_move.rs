//! 无损扩容 Case 2：块级分区移动（仅 PE 内执行）。
//!
//! 当目标卷后方紧邻的不是未分配空间、而是一个**基础数据分区**(如 D:)时，普通扩容
//! 无法把空间并入 C。本模块通过「把后方分区整体向右搬移、在 C 之后腾出未分配空间、再 extend C」
//! 实现真正的无损扩大 C 盘。
//!
//! ## 算法（C 紧跟一个可移动分区 N，N 之后是未分配尾部）
//! 设 C=[c_off, c_off+c_len)，与 C 之间已有未分配 adj；N=[n_off, n_off+n_len)；N 之后空闲 free。
//! 目标把 C 扩大到 target → 需要在 C 之后腾出 delta = target - c_len 的间隙。
//!   - 需要把 N 右移 shift = delta - adj；
//!   - 若 shift > free，先把 N 的文件系统 shrink 掉 (shift - free)，使尾部空出到 shift；
//!   - 右移 N（重叠安全：从高地址向低地址倒序拷贝原始扇区）；
//!   - 通过 VDS 删除旧 N 表项、在新偏移按原大小重建、还原盘符；
//!   - 通过 VDS 把目标卷扩展到 target。
//!
//! ## 安全防呆（任一不满足直接安全失败，不触碰磁盘）
//! - N 必须是 C 之后**紧邻**的、有盘符的**基础数据分区**(非 ESP/MSR/恢复/系统)；
//! - 原始 I/O 的 offset/length/shift 必须满足当前物理磁盘报告的真实逻辑扇区约束；
//!   扇区查询失败或字段矛盾时停止，不能用固定 1 MiB/4 KiB/512 B 猜测；
//! - 搬移前锁定并卸载 N 卷；搬移采用倒序重叠安全拷贝；
//! - 重建分区表项交给 VDS（避免手改 GPT CRC / MBR 出错）；
//! - 全程写 journal 便于诊断；移动数据期间断电会损坏 N（与所有分区工具同理，需提示勿断电）。
//!
//! ⚠️ 本路径会搬移用户数据，必须先在虚拟机/废盘充分验证后再用于真机。

#![cfg(windows)]

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;

use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetVolumeInformationW, ReadFile, SetFilePointerEx, WriteFile, FILE_BEGIN,
    FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, PARTITION_STYLE_GPT,
    PARTITION_STYLE_MBR,
};
use windows::Win32::System::IO::DeviceIoControl;

use crate::core::disk::{DiskManager, PartitionStyle};
use crate::tr;

const MIB: u64 = 1024 * 1024;
const GENERIC_RW: u32 = 0x8000_0000 | 0x4000_0000; // GENERIC_READ | GENERIC_WRITE
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
const COPY_CHUNK: u64 = 4 * MIB;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawMoveDirection {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawMoveIoPlan {
    physical_sector_bytes: usize,
    chunk_bytes: usize,
}

/// Validate only constraints required by the current physical device and the signed Win32 file
/// pointer. `1 MiB` is a layout preference, never a raw-I/O legality rule.
fn plan_raw_move_io(
    geometry: lr_core::windows_storage::DiskSectorGeometry,
    disk_size_bytes: u64,
    src: u64,
    len: u64,
    delta: u64,
    direction: RawMoveDirection,
) -> Result<RawMoveIoPlan> {
    let logical = u64::from(geometry.logical_sector_bytes);
    let physical = u64::from(geometry.physical_sector_bytes);
    if logical == 0
        || physical < logical
        || !physical.is_multiple_of(logical)
        || u64::from(geometry.sector_alignment_offset_bytes) >= physical
        || !u64::from(geometry.sector_alignment_offset_bytes).is_multiple_of(logical)
    {
        bail!("physical disk reported invalid sector geometry");
    }
    if len == 0 || delta == 0 {
        bail!("raw move length and delta must be non-zero");
    }
    for (name, value) in [("source offset", src), ("length", len), ("delta", delta)] {
        if !value.is_multiple_of(logical) {
            bail!(
                "raw move {name} {value} is not aligned to the current logical sector size {logical}"
            );
        }
    }
    let source_end = src
        .checked_add(len)
        .ok_or_else(|| anyhow!("raw move source range overflows"))?;
    let destination_start = match direction {
        RawMoveDirection::Left => src
            .checked_sub(delta)
            .ok_or_else(|| anyhow!("raw move destination starts before disk zero"))?,
        RawMoveDirection::Right => src
            .checked_add(delta)
            .ok_or_else(|| anyhow!("raw move destination offset overflows"))?,
    };
    let destination_end = destination_start
        .checked_add(len)
        .ok_or_else(|| anyhow!("raw move destination range overflows"))?;
    if source_end > disk_size_bytes || destination_end > disk_size_bytes {
        bail!("raw move source or destination is outside the current disk capacity");
    }
    if source_end > i64::MAX as u64 || destination_end > i64::MAX as u64 {
        bail!("raw move range exceeds SetFilePointerEx signed range");
    }

    let preferred_chunk = COPY_CHUNK.max(logical);
    let chunk = preferred_chunk - preferred_chunk % logical;
    let chunk_bytes = usize::try_from(chunk)
        .map_err(|_| anyhow!("raw move chunk does not fit this process address space"))?;
    let physical_sector_bytes = usize::try_from(physical)
        .map_err(|_| anyhow!("physical sector size does not fit this process address space"))?;
    let allocation_bytes = chunk_bytes
        .checked_add(physical_sector_bytes.saturating_sub(1))
        .ok_or_else(|| anyhow!("aligned raw-I/O buffer size overflows"))?;
    if allocation_bytes > isize::MAX as usize || chunk_bytes > u32::MAX as usize {
        bail!("raw-I/O buffer size exceeds Win32 or Rust slice limits");
    }
    Ok(RawMoveIoPlan {
        physical_sector_bytes,
        chunk_bytes,
    })
}

struct AlignedIoBuffer {
    allocation: Vec<u8>,
    start: usize,
    len: usize,
}

impl AlignedIoBuffer {
    fn new(len: usize, alignment: usize) -> Result<Self> {
        if len == 0 || alignment == 0 {
            bail!("aligned raw-I/O buffer requires non-zero size and alignment");
        }
        let allocation_len = len
            .checked_add(alignment - 1)
            .ok_or_else(|| anyhow!("aligned raw-I/O allocation size overflows"))?;
        let mut allocation = Vec::new();
        allocation
            .try_reserve_exact(allocation_len)
            .map_err(|error| anyhow!("allocate raw-I/O buffer failed: {error}"))?;
        allocation.resize(allocation_len, 0);
        let address = allocation.as_ptr() as usize;
        let start = (alignment - address % alignment) % alignment;
        debug_assert_eq!((address + start) % alignment, 0);
        Ok(Self {
            allocation,
            start,
            len,
        })
    }

    fn as_mut_slice(&mut self, len: usize) -> Result<&mut [u8]> {
        if len > self.len {
            bail!("raw-I/O request exceeds aligned buffer capacity");
        }
        Ok(&mut self.allocation[self.start..self.start + len])
    }
}

/// GPT 分区类型 GUID（小端字节序，与 PARTITION_INFORMATION_GPT 一致）。
const BASIC_DATA_GUID: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];
#[repr(C)]
#[derive(Default)]
struct DiskGeometryEx {
    geometry_cylinders: i64,
    geometry_media_type: u32,
    geometry_tracks_per_cylinder: u32,
    geometry_sectors_per_track: u32,
    geometry_bytes_per_sector: u32,
    disk_size: i64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DriveLayoutInfoExHeader {
    partition_style: u32,
    partition_count: u32,
}

fn is_variable_buffer_error(error: &windows::core::Error) -> bool {
    error.code() == HRESULT::from_win32(ERROR_MORE_DATA.0)
        || error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
}

unsafe fn query_drive_layout(handle: HANDLE) -> Option<(Vec<u64>, u32)> {
    let mut capacity = 4096usize;
    loop {
        if capacity > 4 * 1024 * 1024 {
            return None;
        }
        let mut buffer = vec![0u64; capacity.div_ceil(std::mem::size_of::<u64>())];
        let mut returned = 0u32;
        match DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
            None,
            0,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * std::mem::size_of::<u64>()) as u32,
            Some(&mut returned),
            None,
        ) {
            Ok(()) => return Some((buffer, returned)),
            Err(error) if is_variable_buffer_error(&error) => capacity = capacity.checked_mul(2)?,
            Err(_) => return None,
        }
    }
}

/// 单个分区的关键几何信息。
#[derive(Debug, Clone)]
struct PartEntry {
    number: u32,
    offset: u64,
    length: u64,
    is_special: bool, // ESP / MSR / 恢复 等不可随意移动的分区
    mbr_type: Option<u8>,
    mbr_active: bool,
    gpt_metadata: Option<lr_core::windows_storage::GptPartitionMetadata>,
}

/// 读取卷所在物理磁盘号与起始偏移、长度（字节）。
unsafe fn volume_disk_and_offset(letter: char) -> Option<(u32, u64, u64)> {
    let identity = lr_core::windows_storage::volume_identity(letter).ok()?;
    Some((
        identity.disk_number,
        identity.offset_bytes,
        identity.extent_length_bytes,
    ))
}

/// 读取某物理磁盘的分区布局（样式、磁盘可用大小、分区列表）。
unsafe fn read_disk_layout(disk_number: u32) -> Option<(PartitionStyle, u64, Vec<PartEntry>)> {
    let path = format!("\\\\.\\PhysicalDrive{}", disk_number);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = CreateFileW(
        PCWSTR::from_raw(wide.as_ptr()),
        0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        Default::default(),
        None,
    )
    .ok()?;
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut geometry = DiskGeometryEx::default();
    let mut returned: u32 = 0;
    let geo_ok = DeviceIoControl(
        handle,
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
        None,
        0,
        Some(&mut geometry as *mut _ as *mut _),
        std::mem::size_of::<DiskGeometryEx>() as u32,
        Some(&mut returned),
        None,
    );
    if geo_ok.is_err() {
        let _ = CloseHandle(handle);
        return None;
    }
    let disk_size = geometry.disk_size as u64;

    let layout = query_drive_layout(handle);
    let _ = CloseHandle(handle);
    let (buffer, returned) = layout?;
    if returned < std::mem::size_of::<DriveLayoutInfoExHeader>() as u32 {
        return None;
    }

    let buffer = std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), returned as usize);
    let header = std::ptr::read_unaligned(buffer.as_ptr().cast::<DriveLayoutInfoExHeader>());
    let style = if header.partition_style == PARTITION_STYLE_MBR.0 as u32 {
        PartitionStyle::MBR
    } else if header.partition_style == PARTITION_STYLE_GPT.0 as u32 {
        PartitionStyle::GPT
    } else {
        PartitionStyle::Unknown
    };

    // PARTITION_INFORMATION_EX 固定 144 字节；分区数组偏移对 MBR/GPT 都是 48。
    // DRIVE_LAYOUT_INFORMATION_EX 里的 union 按最大 GPT 成员占 40 字节，不随实际分区表类型缩短。
    let entry_size = 144usize;
    let header_size = 48usize;

    let mut parts = Vec::new();
    for i in 0..header.partition_count {
        let off = header_size + i as usize * entry_size;
        if off + entry_size > buffer.len() {
            break;
        }
        let d = &buffer[off..off + entry_size];
        let starting = i64::from_le_bytes(d[8..16].try_into().ok()?);
        let length = i64::from_le_bytes(d[16..24].try_into().ok()?);
        let number = u32::from_le_bytes(d[24..28].try_into().ok()?);
        if length <= 0 {
            continue;
        }
        let mbr_type = (style == PartitionStyle::MBR).then_some(d[32]);
        let mbr_active = style == PartitionStyle::MBR && d[33] != 0;
        let (is_special, gpt_metadata) = if style == PartitionStyle::GPT {
            let mut g = [0u8; 16];
            g.copy_from_slice(&d[32..48]);
            let mut partition_id = [0u8; 16];
            partition_id.copy_from_slice(&d[48..64]);
            let attributes = u64::from_le_bytes(d[64..72].try_into().ok()?);
            let mut name = [0u16; 36];
            for (index, value) in name.iter_mut().enumerate() {
                let offset = 72 + index * 2;
                *value = u16::from_le_bytes(d[offset..offset + 2].try_into().ok()?);
            }
            (
                g != BASIC_DATA_GUID,
                Some(lr_core::windows_storage::GptPartitionMetadata {
                    partition_id,
                    attributes,
                    name,
                }),
            )
        } else {
            // MBR：仅类型 0x07(NTFS/IFS) / 0x0B / 0x0C(FAT32) 视为普通数据，其余保守地当作特殊不移动。
            let t = d[32];
            (!(t == 0x07 || t == 0x0b || t == 0x0c), None)
        };
        parts.push(PartEntry {
            number,
            offset: starting as u64,
            length: length as u64,
            is_special,
            mbr_type,
            mbr_active,
            gpt_metadata,
        });
    }
    parts.sort_by_key(|p| p.offset);
    Some((style, disk_size, parts))
}

/// 锁定并卸载卷，返回持有锁的卷句柄（在移动期间保持打开）。
unsafe fn lock_dismount_volume(letter: char) -> Result<HANDLE> {
    let path = format!("\\\\.\\{}:", letter);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = CreateFileW(
        PCWSTR::from_raw(wide.as_ptr()),
        GENERIC_RW,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        Default::default(),
        None,
    )
    .map_err(|e| anyhow!("{}", tr!("打开卷 {}: 失败: {}", letter, e)))?;
    if handle == INVALID_HANDLE_VALUE {
        bail!("{}", tr!("打开卷 {}: 得到无效句柄", letter));
    }
    let mut returned: u32 = 0;
    if DeviceIoControl(
        handle,
        FSCTL_LOCK_VOLUME,
        None,
        0,
        None,
        0,
        Some(&mut returned),
        None,
    )
    .is_err()
    {
        let _ = CloseHandle(handle);
        bail!("{}", tr!("锁定卷 {}: 失败（可能有句柄占用）", letter));
    }
    if DeviceIoControl(
        handle,
        FSCTL_DISMOUNT_VOLUME,
        None,
        0,
        None,
        0,
        Some(&mut returned),
        None,
    )
    .is_err()
    {
        let _ = CloseHandle(handle);
        bail!("{}", tr!("卸载卷 {}: 失败", letter));
    }
    Ok(handle)
}

/// 在物理磁盘上把 [src, src+len) 整块向右搬移 delta 字节（重叠安全：倒序拷贝）。
unsafe fn raw_move_right(
    disk_number: u32,
    src: u64,
    len: u64,
    delta: u64,
    expected_layout: &lr_core::windows_storage::DiskLayoutSnapshot,
) -> Result<()> {
    let geometry = lr_core::windows_storage::physical_disk_sector_geometry(disk_number)
        .map_err(|error| anyhow!("{}", tr!("查询物理磁盘真实扇区约束失败：{}", error)))?;
    let io_plan = plan_raw_move_io(
        geometry,
        expected_layout.disk_size_bytes,
        src,
        len,
        delta,
        RawMoveDirection::Right,
    )?;
    let path = format!("\\\\.\\PhysicalDrive{}", disk_number);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = CreateFileW(
        PCWSTR::from_raw(wide.as_ptr()),
        GENERIC_RW,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH,
        None,
    )
    .map_err(|e| anyhow!("{}", tr!("打开物理磁盘 {} 失败: {}", disk_number, e)))?;
    if handle == INVALID_HANDLE_VALUE {
        bail!("{}", tr!("打开物理磁盘 {} 得到无效句柄", disk_number));
    }

    let result = (|| -> Result<()> {
        lr_core::windows_storage::verify_disk_layout_snapshot_from_physical_handle(
            handle,
            expected_layout,
        )
        .map_err(|error| anyhow!("{}", tr!("物理磁盘在原始搬移写入前已变化：{}", error)))?;
        let mut buffer = AlignedIoBuffer::new(io_plan.chunk_bytes, io_plan.physical_sector_bytes)?;
        let mut pos = len; // 已处理到区域内的字节位置（从尾部往头部）
        while pos > 0 {
            let this = (io_plan.chunk_bytes as u64).min(pos);
            let rel = pos - this;
            let read_at = i64::try_from(src + rel)
                .map_err(|_| anyhow!("raw read offset exceeds SetFilePointerEx range"))?;
            let write_at = i64::try_from(src + delta + rel)
                .map_err(|_| anyhow!("raw write offset exceeds SetFilePointerEx range"))?;
            let io_buffer = buffer.as_mut_slice(this as usize)?;

            // 读
            seek(handle, read_at)?;
            read_exact(handle, io_buffer)?;
            // 写
            seek(handle, write_at)?;
            write_exact(handle, io_buffer)?;

            pos -= this;
        }
        // 刷盘
        windows::Win32::Storage::FileSystem::FlushFileBuffers(handle)
            .map_err(|e| anyhow!("{}", tr!("刷盘失败: {}", e)))?;
        Ok(())
    })();

    let _ = CloseHandle(handle);
    result
}

/// 在物理磁盘上把 `[src, src+len)` 整块向左搬移 `delta` 字节。
///
/// 目标区间起点低于源区间，重叠时必须从低地址向高地址正序复制。
unsafe fn raw_move_left(
    disk_number: u32,
    src: u64,
    len: u64,
    delta: u64,
    expected_layout: &lr_core::windows_storage::DiskLayoutSnapshot,
) -> Result<()> {
    if delta == 0 || src < delta {
        bail!("{}", tr!("向左搬移参数无效"));
    }
    let geometry = lr_core::windows_storage::physical_disk_sector_geometry(disk_number)
        .map_err(|error| anyhow!("{}", tr!("查询物理磁盘真实扇区约束失败：{}", error)))?;
    let io_plan = plan_raw_move_io(
        geometry,
        expected_layout.disk_size_bytes,
        src,
        len,
        delta,
        RawMoveDirection::Left,
    )?;
    let path = format!("\\\\.\\PhysicalDrive{}", disk_number);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = CreateFileW(
        PCWSTR::from_raw(wide.as_ptr()),
        GENERIC_RW,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH,
        None,
    )
    .map_err(|e| anyhow!("{}", tr!("打开物理磁盘 {} 失败: {}", disk_number, e)))?;
    if handle == INVALID_HANDLE_VALUE {
        bail!("{}", tr!("打开物理磁盘 {} 得到无效句柄", disk_number));
    }

    let result = (|| -> Result<()> {
        lr_core::windows_storage::verify_disk_layout_snapshot_from_physical_handle(
            handle,
            expected_layout,
        )
        .map_err(|error| anyhow!("{}", tr!("物理磁盘在原始搬移写入前已变化：{}", error)))?;
        let mut buffer = AlignedIoBuffer::new(io_plan.chunk_bytes, io_plan.physical_sector_bytes)?;
        let mut position = 0_u64;
        while position < len {
            let this = (io_plan.chunk_bytes as u64).min(len - position);
            let read_at = i64::try_from(src + position)
                .map_err(|_| anyhow!("raw read offset exceeds SetFilePointerEx range"))?;
            let write_at = i64::try_from(src - delta + position)
                .map_err(|_| anyhow!("raw write offset exceeds SetFilePointerEx range"))?;
            let io_buffer = buffer.as_mut_slice(this as usize)?;
            seek(handle, read_at)?;
            read_exact(handle, io_buffer)?;
            seek(handle, write_at)?;
            write_exact(handle, io_buffer)?;
            position += this;
        }
        windows::Win32::Storage::FileSystem::FlushFileBuffers(handle)
            .map_err(|e| anyhow!("{}", tr!("刷盘失败: {}", e)))?;
        Ok(())
    })();
    let _ = CloseHandle(handle);
    result
}

unsafe fn seek(handle: HANDLE, offset: i64) -> Result<()> {
    SetFilePointerEx(handle, offset, None, FILE_BEGIN)
        .map_err(|e| anyhow!("{}", tr!("定位到 {} 失败: {}", offset, e)))
}

unsafe fn read_exact(handle: HANDLE, buf: &mut [u8]) -> Result<()> {
    let mut read: u32 = 0;
    ReadFile(handle, Some(buf), Some(&mut read), None)
        .map_err(|e| anyhow!("{}", tr!("读盘失败: {}", e)))?;
    if read as usize != buf.len() {
        bail!(
            "{}",
            tr!(
                "读盘返回短读取（期望 {} 字节，实际 {} 字节）",
                buf.len(),
                read
            )
        );
    }
    Ok(())
}

unsafe fn write_exact(handle: HANDLE, buf: &[u8]) -> Result<()> {
    let mut written: u32 = 0;
    WriteFile(handle, Some(buf), Some(&mut written), None)
        .map_err(|e| anyhow!("{}", tr!("写盘失败: {}", e)))?;
    if written as usize != buf.len() {
        bail!(
            "{}",
            tr!(
                "写盘返回短写入（期望 {} 字节，实际 {} 字节，磁盘可能处于部分写入状态）",
                buf.len(),
                written
            )
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeftTransferPlan {
    delta: u64,
    gap_before_target: u64,
    donor_shrink_by: u64,
    donor_length_after_shrink: u64,
    target_new_offset: u64,
}

fn plan_left_transfer(
    donor_offset: u64,
    donor_length: u64,
    target_offset: u64,
    target_length: u64,
    requested_target_length: u64,
) -> Result<LeftTransferPlan> {
    let donor_end = donor_offset
        .checked_add(donor_length)
        .ok_or_else(|| anyhow!("donor geometry overflow"))?;
    if donor_end > target_offset {
        bail!("donor and target partitions overlap");
    }
    if requested_target_length <= target_length {
        bail!("target length must grow");
    }
    let delta = requested_target_length - target_length;
    let gap_before_target = target_offset - donor_end;
    let donor_shrink_by = delta.saturating_sub(gap_before_target);
    if donor_shrink_by >= donor_length || target_offset < delta {
        bail!("insufficient geometry for left-side transfer");
    }
    let donor_length_after_shrink = donor_length - donor_shrink_by;
    let target_new_offset = target_offset - delta;
    let donor_new_end = donor_offset
        .checked_add(donor_length_after_shrink)
        .ok_or_else(|| anyhow!("shrunken donor geometry overflow"))?;
    if donor_new_end > target_new_offset {
        bail!("shrunken donor still overlaps the relocated target");
    }
    Ok(LeftTransferPlan {
        delta,
        gap_before_target,
        donor_shrink_by,
        donor_length_after_shrink,
        target_new_offset,
    })
}

fn validate_left_transfer_identity(
    config: &crate::core::config::ExpandConfig,
    disk_number: u32,
    disk_size: u64,
    target: &PartEntry,
    donor: &PartEntry,
) -> Result<()> {
    if config.expected_disk_size_bytes == 0
        || config.expected_partition_number == 0
        || config.expected_partition_offset_bytes == 0
        || config.expected_partition_size_bytes == 0
        || config.expected_donor_partition_number == 0
        || config.expected_donor_offset_bytes == 0
        || config.expected_donor_size_bytes == 0
    {
        bail!("{}", tr!("左侧空间转移配置缺少完整磁盘/分区身份，拒绝写盘"));
    }
    if disk_number != config.expected_disk_number
        || disk_size != config.expected_disk_size_bytes
        || target.number != config.expected_partition_number
        || target.offset != config.expected_partition_offset_bytes
        || target.length != config.expected_partition_size_bytes
        || donor.number != config.expected_donor_partition_number
        || donor.offset != config.expected_donor_offset_bytes
        || donor.length != config.expected_donor_size_bytes
    {
        bail!("{}", tr!("重启后磁盘或左右分区身份/几何已变化，拒绝写盘"));
    }
    if donor
        .offset
        .checked_add(donor.length)
        .is_none_or(|end| end != target.offset)
    {
        bail!("{}", tr!("左侧供体分区不再与目标分区直接相邻，拒绝写盘"));
    }
    Ok(())
}

/// 扫描 C..Z，找出位于指定磁盘且起始偏移匹配的卷盘符。
fn letter_for(disk: u32, offset: u64) -> Option<char> {
    for l in b'C'..=b'Z' {
        let c = l as char;
        if !Path::new(&format!("{}:\\", c)).exists() {
            continue;
        }
        if let Some((d, off, _len)) = unsafe { volume_disk_and_offset(c) } {
            if d == disk && off == offset {
                return Some(c);
            }
        }
    }
    None
}

fn volume_file_system(letter: char) -> Result<String> {
    let root = format!("{}:\\", letter.to_ascii_uppercase());
    let wide = root
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut file_system = [0_u16; 32];
    unsafe {
        GetVolumeInformationW(
            PCWSTR(wide.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut file_system),
        )
        .map_err(|error| anyhow!("{}", tr!("读取分区 {}: 文件系统失败: {}", letter, error)))?;
    }
    Ok(String::from_utf16_lossy(&file_system)
        .trim_end_matches('\0')
        .to_string())
}

/// 写一行 journal 便于失败诊断（best-effort）。
fn journal(data_partition: &str, line: &str) {
    let dir = format!("{}\\LetRecovery_Data", data_partition);
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{}\\expand_move.journal", dir);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

fn plan_right_donor_shrink(
    target_current_bytes: u64,
    target_final_bytes: u64,
    gap_before_donor_bytes: u64,
    donor_current_bytes: u64,
    shift_bytes: u64,
    free_after_donor_bytes: u64,
    donor_target_bytes: u64,
) -> Result<u64> {
    let minimum_shrink = shift_bytes.saturating_sub(free_after_donor_bytes);
    if donor_target_bytes == 0 {
        return Ok(minimum_shrink);
    }
    if gap_before_donor_bytes != 0
        || donor_target_bytes >= donor_current_bytes
        || target_final_bytes.checked_add(donor_target_bytes)
            != target_current_bytes.checked_add(donor_current_bytes)
    {
        bail!(
            "{}",
            tr!("相邻分区转移的目标/供体最终大小与当前布局不一致，拒绝写盘")
        );
    }
    let exact_shrink = donor_current_bytes - donor_target_bytes;
    if exact_shrink < minimum_shrink {
        bail!(
            "{}",
            tr!("供体分区的计划收缩量不足以容纳移动后的分区，拒绝写盘")
        );
    }
    Ok(exact_shrink)
}

fn observed_stable_shrink_bytes(
    before: lr_core::windows_storage::StableVolumeIdentity,
    current: lr_core::windows_storage::StableVolumeIdentity,
) -> Result<Option<u64>> {
    if lr_core::windows_storage::same_stable_volume_identity(before, current) {
        return Ok(None);
    }
    if !lr_core::windows_storage::same_stable_partition_identity(before, current) {
        bail!("current drive letter no longer names the authenticated pre-shrink partition");
    }
    if current.extent.disk_number != before.extent.disk_number
        || current.extent.offset_bytes != before.extent.offset_bytes
    {
        bail!("current partition no longer starts at the authenticated pre-shrink extent");
    }
    let reclaimed = before
        .extent
        .extent_length_bytes
        .checked_sub(current.extent.extent_length_bytes)
        .ok_or_else(|| anyhow!("current partition grew instead of forming a recoverable shrink"))?;
    if reclaimed == 0 {
        return Ok(None);
    }
    Ok(Some(reclaimed))
}

/// Once VDS accepted an asynchronous shrink, a Wait/Refresh/readback error does not prove that the
/// volume remained unchanged. Recovery is allowed only from a fresh authoritative read of the same
/// stable partition and uses the actually observed tail reduction, never the requested byte count.
fn recover_observed_shrink(
    letter: char,
    before: lr_core::windows_storage::StableVolumeIdentity,
) -> String {
    let current = match lr_core::windows_storage::stable_volume_identity(letter) {
        Ok(current) => current,
        Err(error) => {
            return format!("authoritative re-read failed; no blind extension attempted: {error}")
        }
    };
    let reclaimed = match observed_stable_shrink_bytes(before, current) {
        Ok(None) => return "authoritative readback shows no extent change; no recovery needed".to_owned(),
        Ok(Some(reclaimed)) => reclaimed,
        Err(error) => {
            return format!(
                "current object is not a provable tail shrink of the authenticated partition; no blind extension attempted: {error:#}"
            )
        }
    };
    match lr_core::windows_storage::extend_volume_stable_checked(letter, current, reclaimed) {
        Ok(()) => match lr_core::windows_storage::stable_volume_identity(letter) {
            Ok(restored)
                if lr_core::windows_storage::same_stable_volume_identity(restored, before) =>
            {
                format!("restored the authoritative observed shrink of {reclaimed} bytes")
            }
            Ok(restored) => format!(
                "extension returned success but final authoritative extent differs from the original: {:?}",
                restored.extent
            ),
            Err(error) => format!("extension returned success but final readback failed: {error}"),
        },
        Err(error) => format!(
            "observed an actual shrink of {reclaimed} bytes but safe extension recovery failed: {error}"
        ),
    }
}

fn shrink_with_observed_recovery(
    letter: char,
    before: lr_core::windows_storage::StableVolumeIdentity,
    desired_bytes: u64,
    minimum_bytes: u64,
) -> Result<u64> {
    match lr_core::windows_storage::shrink_volume_stable_checked(
        letter,
        before,
        desired_bytes,
        minimum_bytes,
    ) {
        Ok(reclaimed) => Ok(reclaimed),
        Err(error) => {
            let recovery = recover_observed_shrink(letter, before);
            bail!("VDS shrink failed or ended in an uncertain state: {error}; recovery result: {recovery}")
        }
    }
}

fn pre_move_error_with_shrink_recovery(
    letter: char,
    before: Option<lr_core::windows_storage::StableVolumeIdentity>,
    error: anyhow::Error,
) -> anyhow::Error {
    match before {
        Some(before) => anyhow!(
            "{}; no raw write had started, shrink recovery result: {}",
            error,
            recover_observed_shrink(letter, before)
        ),
        None => error,
    }
}

fn recreated_partition_matches_raw_range(
    created: lr_core::windows_storage::CreatedPartition,
    raw_offset_bytes: u64,
    raw_length_bytes: u64,
) -> bool {
    created.offset_bytes == raw_offset_bytes && created.size_bytes == raw_length_bytes
}

/// Raw bytes were moved to one exact range, so a provider-adjusted partition entry cannot be
/// accepted even if it overlaps or contains that range: changing the file-system start/end would
/// expose the wrong sectors. Cleanup is authorized only by the returned actual extent plus a fresh
/// canonical snapshot; requested geometry is never used to guess which entry to delete.
fn reject_and_cleanup_mismatched_recreate(
    disk_number: u32,
    created: lr_core::windows_storage::CreatedPartition,
    raw_offset_bytes: u64,
    raw_length_bytes: u64,
    data_partition: &str,
) -> Result<()> {
    if recreated_partition_matches_raw_range(created, raw_offset_bytes, raw_length_bytes) {
        return Ok(());
    }
    journal(
        data_partition,
        &format!(
            "RECREATE MISMATCH raw_off={} raw_len={} actual_off={} actual_len={}",
            raw_offset_bytes, raw_length_bytes, created.offset_bytes, created.size_bytes
        ),
    );
    let current_layout = lr_core::windows_storage::disk_layout_snapshot(disk_number).map_err(
        |error| {
            anyhow!(
                "raw data remains at exact range [{}+{}], but provider created mismatched partition entry [{}+{}] and canonical cleanup snapshot failed; preserve partial state: {}",
                raw_offset_bytes,
                raw_length_bytes,
                created.offset_bytes,
                created.size_bytes,
                error
            )
        },
    )?;
    match lr_core::windows_storage::delete_partition_checked(
        disk_number,
        created.offset_bytes,
        true,
        &current_layout,
    ) {
        Ok(()) => bail!(
            "provider created mismatched partition entry [{}+{}]; it was removed using its actual canonical extent. Raw data remains at exact range [{}+{}] without a partition entry; preserve journal and repair manually",
            created.offset_bytes,
            created.size_bytes,
            raw_offset_bytes,
            raw_length_bytes
        ),
        Err(error) => bail!(
            "provider created mismatched partition entry [{}+{}] for raw data at [{}+{}], and checked cleanup of that actual entry failed; preserve partial state: {}",
            created.offset_bytes,
            created.size_bytes,
            raw_offset_bytes,
            raw_length_bytes,
            error
        ),
    }
}

/// 编排：把分区 `letter` 无损扩大到配置指定大小（0=尽量并入相邻未分配空间）。
///
/// 优先 Case 1（WinAPI extend 并入相邻未分配空间）；不足时尝试 Case 2（移动紧邻的基础数据分区）。
/// `data_partition` 仅用于写 journal。
pub fn expand_c_drive(
    letter: char,
    config: &crate::core::config::ExpandConfig,
    data_partition: &str,
    expected_target: lr_core::windows_storage::VolumeIdentity,
) -> Result<String> {
    let target_size_mb = config.target_size_mb;
    // 0=尽量扩到相邻未分配空间最大 → 直接 Case 1。
    if target_size_mb == 0 {
        return DiskManager::expand_partition_lossless_checked(letter, 0, expected_target)
            .map_err(|e| anyhow!(e));
    }

    DiskManager::verify_partition_volume_identity(&format!("{}:", letter), expected_target)
        .context("authenticated expand target changed before layout planning")?;

    let (disk, c_off, c_len) = unsafe { volume_disk_and_offset(letter) }
        .ok_or_else(|| anyhow!("{}", tr!("无法定位分区 {}: 所在磁盘/偏移", letter)))?;
    let target_bytes = target_size_mb * MIB;
    if target_bytes <= c_len {
        return Ok(tr!("分区 {}: 当前已达到或超过目标大小，无需扩容", letter));
    }
    let delta = target_bytes - c_len;

    let (style, disk_size, parts) = unsafe { read_disk_layout(disk) }
        .ok_or_else(|| anyhow!("{}", tr!("读取磁盘 {} 布局失败", disk)))?;
    if style == PartitionStyle::Unknown {
        bail!("{}", tr!("磁盘 {} 分区表类型未知，拒绝操作", disk));
    }

    // 找到 C 之后紧邻的分区 N（offset 最小且 > c_off）。
    let c_end = c_off + c_len;
    let target_layout = parts
        .iter()
        .find(|partition| partition.offset == c_off && partition.length == c_len)
        .ok_or_else(|| anyhow!("{}", tr!("磁盘布局中未找到目标分区，拒绝操作")))?;
    let next = parts
        .iter()
        .filter(|p| p.offset >= c_end)
        .min_by_key(|p| p.offset);
    let adj_unalloc = match next {
        Some(n) => n.offset.saturating_sub(c_end),
        None => disk_size.saturating_sub(c_end),
    };

    // 相邻未分配空间已够 → Case 1。
    if config.donor_target_size_mb == 0 && delta <= adj_unalloc {
        return DiskManager::expand_partition_lossless_checked(
            letter,
            target_size_mb,
            expected_target,
        )
        .map_err(|e| anyhow!(e));
    }

    if config.donor_target_size_mb != 0 || delta > adj_unalloc {
        bail!(
            "partition-moving expansion is disabled until the authenticated canonical target and donor layout can remain pinned through every raw-write stage"
        );
    }

    // 否则需要移动后方分区（Case 2）。
    let n = next.ok_or_else(|| {
        anyhow!(
            "{}",
            tr!(
                "C 盘后方空间不足且无可移动分区（相邻未分配仅 {} MiB）",
                adj_unalloc / MIB
            )
        )
    })?;

    // ===== 防呆校验（任一不满足，安全失败，不触碰磁盘）=====
    if config.donor_target_size_mb > 0
        && (config.expected_disk_number == 0
            || config.expected_disk_size_bytes == 0
            || config.expected_partition_number == 0
            || config.expected_partition_offset_bytes == 0
            || config.expected_partition_size_bytes == 0
            || config.expected_donor_partition_number == 0
            || config.expected_donor_offset_bytes == 0
            || config.expected_donor_size_bytes == 0
            || disk != config.expected_disk_number
            || disk_size != config.expected_disk_size_bytes
            || target_layout.number != config.expected_partition_number
            || c_off != config.expected_partition_offset_bytes
            || c_len != config.expected_partition_size_bytes
            || n.number != config.expected_donor_partition_number
            || n.offset != config.expected_donor_offset_bytes
            || n.length != config.expected_donor_size_bytes)
    {
        bail!("{}", tr!("重启后磁盘或相邻分区身份/几何已变化，拒绝写盘"));
    }
    if n.is_special {
        bail!(
            "{}",
            tr!("C 盘后方分区是系统/ESP/MSR/恢复等特殊分区，为安全起见拒绝移动")
        );
    }
    if style == PartitionStyle::MBR && n.mbr_type != Some(0x07) {
        bail!(
            "{}",
            tr!("后方 MBR 分区类型不是受支持的 NTFS 基础数据类型（0x07），拒绝移动")
        );
    }
    // N 必须紧贴 C（中间最多只有已计入的 adj_unalloc）。
    if n.offset != c_end + adj_unalloc {
        bail!("{}", tr!("分区布局异常（后方分区不连续），拒绝移动"));
    }
    // N 必须有盘符（用于卸载与重建后还原）。
    let n_letter = letter_for(disk, n.offset)
        .ok_or_else(|| anyhow!("{}", tr!("后方分区无盘符，无法安全移动")))?;
    // N 后方边界（下一分区起点或磁盘尾）。
    let after_n = parts
        .iter()
        .filter(|p| p.offset > n.offset)
        .map(|p| p.offset)
        .min()
        .unwrap_or(disk_size);
    let n_end = n.offset + n.length;
    let free_after_n = after_n.saturating_sub(n_end);

    // 需要把 N 右移 shift，使 C 之后间隙达到 delta。
    let shift = delta - adj_unalloc;
    // 普通扩容只收缩放不下的部分；分区图的成对转移必须精确收缩到用户规划的
    // 供体最终大小，不能优先吞掉供体后方原本应保留的未分配空间。
    let donor_target_bytes = config
        .donor_target_size_mb
        .checked_mul(MIB)
        .ok_or_else(|| anyhow!("{}", tr!("供体分区目标大小溢出")))?;
    let shrink_by = plan_right_donor_shrink(
        c_len,
        target_bytes,
        adj_unalloc,
        n.length,
        shift,
        free_after_n,
        donor_target_bytes,
    )?;

    // 在仍可逆的 shrink 之前，先按设备实际逻辑扇区验证原始搬移几何。provider 后续可能
    // 按文件系统 cluster 合法取整 shrink；首次原始写入前还会用实际回读长度再次验证。
    let sector_geometry = lr_core::windows_storage::physical_disk_sector_geometry(disk)
        .map_err(|error| anyhow!("{}", tr!("查询物理磁盘真实扇区约束失败：{}", error)))?;
    plan_raw_move_io(
        sector_geometry,
        disk_size,
        n.offset,
        n.length,
        shift,
        RawMoveDirection::Right,
    )?;

    journal(
        data_partition,
        &format!(
            "PLAN disk={} C[{}+{}] N#{}[{}+{}] letter={} adj={} delta={} shift={} shrink={} free_after={}",
            disk, c_off, c_len, n.number, n.offset, n.length, n_letter,
            adj_unalloc, delta, shift, shrink_by, free_after_n
        ),
    );
    log::warn!(
        "[EXPAND-MOVE] 计划：磁盘{} 移动分区#{}({}:) 右移 {} MiB（必要时先 shrink {} MiB），再扩 C",
        disk,
        n.number,
        n_letter,
        shift / MIB,
        shrink_by / MIB
    );

    // ===== Step A：必要时 shrink N 文件系统 =====
    let mut n_len_now = n.length;
    let mut shrink_before = None;
    if shrink_by > 0 {
        journal(
            data_partition,
            &format!("SHRINK {}: by {} MiB", n_letter, shrink_by / MIB),
        );
        let expected_extent = lr_core::windows_storage::VolumeIdentity {
            disk_number: disk,
            offset_bytes: n.offset,
            extent_length_bytes: n.length,
        };
        let expected = lr_core::windows_storage::stable_volume_identity(n_letter)?;
        if !lr_core::windows_storage::same_volume_identity(expected.extent, expected_extent) {
            bail!("donor partition stable identity changed before shrink");
        }
        shrink_before = Some(expected);
        let reclaimed = shrink_with_observed_recovery(n_letter, expected, shrink_by, shrink_by)
            .map_err(|error| anyhow!("{}", tr!("收缩后方分区 {}: 失败：{}", n_letter, error)))?;
        log::debug!(
            "VDS shrink requested {} bytes and authoritative readback proved {} bytes",
            shrink_by,
            reclaimed
        );
        // 重新读取布局，确认同一分区的实际新范围；不能要求 provider 与请求逐字节相等。
        let (_s2, _ds2, parts2) = unsafe { read_disk_layout(disk) }
            .ok_or_else(|| anyhow!("{}", tr!("shrink 后重读磁盘布局失败")))
            .map_err(|error| pre_move_error_with_shrink_recovery(n_letter, shrink_before, error))?;
        let n2 = parts2
            .iter()
            .find(|p| p.number == n.number && p.offset == n.offset)
            .ok_or_else(|| anyhow!("{}", tr!("shrink 后未找到原分区，已中止（未移动数据）")))
            .map_err(|error| pre_move_error_with_shrink_recovery(n_letter, shrink_before, error))?;
        n_len_now = n2.length;
        // 再次确认右移后能放下：n.offset+shift+n_len_now <= after_n
        let relocated_end = n
            .offset
            .checked_add(shift)
            .and_then(|value| value.checked_add(n_len_now));
        if relocated_end.is_none_or(|end| end > after_n) {
            return Err(pre_move_error_with_shrink_recovery(
                n_letter,
                shrink_before,
                anyhow!("{}", tr!("shrink 后空间仍不足，已中止（未移动数据）")),
            ));
        }
    }

    // ===== Step B：锁定/卸载 N，倒序重叠安全搬移 =====
    let move_identity =
        lr_core::windows_storage::stable_volume_identity(n_letter).map_err(|error| {
            pre_move_error_with_shrink_recovery(n_letter, shrink_before, anyhow!(error))
        })?;
    if move_identity.extent.disk_number != disk
        || move_identity.extent.offset_bytes != n.offset
        || move_identity.extent.extent_length_bytes != n_len_now
    {
        return Err(pre_move_error_with_shrink_recovery(
            n_letter,
            shrink_before,
            anyhow!("donor stable identity changed before raw move"),
        ));
    }
    let move_layout = lr_core::windows_storage::disk_layout_snapshot(disk).map_err(|error| {
        pre_move_error_with_shrink_recovery(n_letter, shrink_before, anyhow!(error))
    })?;
    journal(
        data_partition,
        &format!(
            "MOVE start n_off={} len={} shift={}",
            n.offset, n_len_now, shift
        ),
    );
    let vol_handle = unsafe { lock_dismount_volume(n_letter) }
        .map_err(|error| pre_move_error_with_shrink_recovery(n_letter, shrink_before, error))?;
    let move_res = unsafe { raw_move_right(disk, n.offset, n_len_now, shift, &move_layout) };
    unsafe {
        let _ = CloseHandle(vol_handle);
    }
    move_res.map_err(|e| {
        journal(data_partition, &format!("MOVE FAILED: {}", e));
        anyhow!(
            "{}",
            tr!(
                "搬移分区数据失败（分区 {} 可能已损坏，请用 journal 诊断）：{}",
                n_letter,
                e
            )
        )
    })?;
    journal(data_partition, "MOVE done");

    // ===== Step C：VDS 删除旧表项、按原大小在新偏移重建、还原盘符 =====
    let new_off = n.offset + shift;
    journal(
        data_partition,
        &format!(
            "RECREATE off={} size={} letter={}",
            new_off, n_len_now, n_letter
        ),
    );
    lr_core::windows_storage::delete_partition_checked(disk, n.offset, true, &move_layout)
        .map_err(|error| {
        anyhow!(
            "{}",
            tr!(
                "搬移已完成但删除旧分区表项失败（分区 {} 数据在新位置 offset={}，请据 journal 修复）：{}",
                n_letter,
                new_off,
                error
            )
        )
        })?;
    let recreate_layout = lr_core::windows_storage::disk_layout_snapshot(disk)?;
    let created = lr_core::windows_storage::create_partition_checked(
        &lr_core::windows_storage::CreatePartitionRequest {
            disk_number: disk,
            offset_bytes: new_off,
            size_bytes: n_len_now,
            kind: lr_core::windows_storage::PartitionKind::BasicData,
            file_system: None,
            label: String::new(),
            drive_letter: Some(n_letter),
            active: style == PartitionStyle::MBR && n.mbr_active,
            preserve_gpt_metadata: n.gpt_metadata.clone(),
        },
        &recreate_layout,
    )
    .map_err(|error| {
        anyhow!(
            "{}",
            tr!(
                "搬移已完成但重建分区表项失败（分区 {} 数据在新位置 offset={} 但表项未建好，请据 journal 修复）：{}",
                n_letter,
                new_off,
                error
            )
        )
    })?;
    reject_and_cleanup_mismatched_recreate(disk, created, new_off, n_len_now, data_partition)?;

    // ===== Step D：把 C extend 到目标 =====
    journal(data_partition, "EXTEND C");
    let msg =
        DiskManager::expand_partition_lossless_checked(letter, target_size_mb, expected_target)
            .map_err(|e| {
                anyhow!(
                    "{}",
                    tr!(
                "分区已成功移动，但最后扩展 C 失败：{}（可重试一键扩容，此时已是相邻未分配空间）",
                e
            )
                )
            })?;
    journal(data_partition, "DONE");
    Ok(tr!("已移动后方分区 {} 并{}", n_letter, msg))
}

/// 从目标分区左侧紧邻的普通数据分区转移容量。
///
/// 该路径只在 PE 中使用：先精确收缩左侧卷，再锁定并卸载目标卷，将目标卷原始字节
/// 正序向左搬移，最后以保留的 GPT 元数据/MBR 活动状态重建表项并扩展尾部。
pub fn expand_from_left_donor(
    letter: char,
    config: &crate::core::config::ExpandConfig,
    data_partition: &str,
    expected_target: lr_core::windows_storage::VolumeIdentity,
    authenticated_control_handles_released: bool,
) -> Result<String> {
    if !authenticated_control_handles_released {
        bail!(
            "left-side expansion is disabled because authenticated control handles are still open on the target volume"
        );
    }
    let target_size_mb = config.target_size_mb;
    if target_size_mb == 0 {
        bail!("{}", tr!("从左侧转移空间必须指定明确的目标大小"));
    }
    let (disk, target_offset, target_length) = unsafe { volume_disk_and_offset(letter) }
        .ok_or_else(|| anyhow!("{}", tr!("无法定位分区 {}: 所在磁盘/偏移", letter)))?;
    let requested_target_length = target_size_mb
        .checked_mul(MIB)
        .ok_or_else(|| anyhow!("{}", tr!("目标分区大小溢出")))?;
    if requested_target_length <= target_length {
        return Ok(tr!("分区 {}: 当前已达到或超过目标大小，无需扩容", letter));
    }

    let (style, disk_size, parts) = unsafe { read_disk_layout(disk) }
        .ok_or_else(|| anyhow!("{}", tr!("读取磁盘 {} 布局失败", disk)))?;
    if style == PartitionStyle::Unknown {
        bail!("{}", tr!("磁盘 {} 分区表类型未知，拒绝操作", disk));
    }
    let target = parts
        .iter()
        .find(|partition| partition.offset == target_offset && partition.length == target_length)
        .ok_or_else(|| anyhow!("{}", tr!("分区表中未找到目标分区 {}:", letter)))?;
    let donor = parts
        .iter()
        .filter(|partition| {
            partition
                .offset
                .checked_add(partition.length)
                .is_some_and(|end| end <= target_offset)
        })
        .max_by_key(|partition| partition.offset)
        .ok_or_else(|| anyhow!("{}", tr!("目标分区左侧没有可转移空间的分区")))?;
    validate_left_transfer_identity(config, disk, disk_size, target, donor)?;
    if target.is_special || donor.is_special {
        bail!(
            "{}",
            tr!("目标分区或左侧分区不是普通 GPT 基础数据/MBR NTFS 分区，拒绝移动")
        );
    }
    if style == PartitionStyle::MBR
        && (target.mbr_type != Some(0x07) || donor.mbr_type != Some(0x07))
    {
        bail!("{}", tr!("仅支持在 MBR 0x07 NTFS 基础数据分区之间转移空间"));
    }
    let donor_letter = letter_for(disk, donor.offset)
        .ok_or_else(|| anyhow!("{}", tr!("左侧分区无盘符，无法安全收缩和复核")))?;
    if !volume_file_system(letter)?.eq_ignore_ascii_case("NTFS")
        || !volume_file_system(donor_letter)?.eq_ignore_ascii_case("NTFS")
    {
        bail!("{}", tr!("左右分区必须仍为 NTFS，拒绝收缩或搬移"));
    }
    let plan = plan_left_transfer(
        donor.offset,
        donor.length,
        target.offset,
        target.length,
        requested_target_length,
    )
    .map_err(|error| anyhow!("{}", tr!("无法规划从左侧转移空间: {}", error)))?;
    // Reject unsupported raw-I/O geometry while the volume layout is still untouched. Legal
    // sector-aligned but non-MiB geometry is accepted.
    let sector_geometry = lr_core::windows_storage::physical_disk_sector_geometry(disk)
        .map_err(|error| anyhow!("{}", tr!("查询物理磁盘真实扇区约束失败：{}", error)))?;
    plan_raw_move_io(
        sector_geometry,
        disk_size,
        target.offset,
        target.length,
        plan.delta,
        RawMoveDirection::Left,
    )?;

    journal(
        data_partition,
        &format!(
            "PLAN-LEFT disk={} donor#{}({}:)[{}+{}] target#{}({}:)[{}+{}] delta={} gap={} shrink={} new_target_off={}",
            disk,
            donor.number,
            donor_letter,
            donor.offset,
            donor.length,
            target.number,
            letter,
            target.offset,
            target.length,
            plan.delta,
            plan.gap_before_target,
            plan.donor_shrink_by,
            plan.target_new_offset
        ),
    );

    let mut shrink_before = None;
    if plan.donor_shrink_by > 0 {
        let expected_extent = lr_core::windows_storage::VolumeIdentity {
            disk_number: disk,
            offset_bytes: donor.offset,
            extent_length_bytes: donor.length,
        };
        let expected = lr_core::windows_storage::stable_volume_identity(donor_letter)?;
        if !lr_core::windows_storage::same_volume_identity(expected.extent, expected_extent) {
            bail!("donor partition stable identity changed before shrink");
        }
        shrink_before = Some(expected);
        let reclaimed = shrink_with_observed_recovery(
            donor_letter,
            expected,
            plan.donor_shrink_by,
            plan.donor_shrink_by,
        )
        .map_err(|error| anyhow!("{}", tr!("收缩左侧分区 {}: 失败：{}", donor_letter, error)))?;
        log::debug!(
            "left donor shrink requested {} bytes and authoritative readback proved {} bytes",
            plan.donor_shrink_by,
            reclaimed
        );
    }

    let (_style_after_shrink, _size_after_shrink, fresh_parts) = unsafe { read_disk_layout(disk) }
        .ok_or_else(|| anyhow!("{}", tr!("收缩后重读磁盘布局失败")))
        .map_err(|error| pre_move_error_with_shrink_recovery(donor_letter, shrink_before, error))?;
    let fresh_donor = fresh_parts
        .iter()
        .find(|partition| partition.number == donor.number && partition.offset == donor.offset)
        .ok_or_else(|| {
            anyhow!(
                "{}",
                tr!("收缩后未找到原左侧分区，已中止（未移动目标数据）")
            )
        })
        .map_err(|error| pre_move_error_with_shrink_recovery(donor_letter, shrink_before, error))?;
    let fresh_target = fresh_parts
        .iter()
        .find(|partition| {
            partition.number == target.number
                && partition.offset == target.offset
                && partition.length == target.length
        })
        .ok_or_else(|| {
            anyhow!(
                "{}",
                tr!("收缩后目标分区布局已变化，已中止（未移动目标数据）")
            )
        })
        .map_err(|error| pre_move_error_with_shrink_recovery(donor_letter, shrink_before, error))?;
    if fresh_donor.length > plan.donor_length_after_shrink
        || fresh_donor
            .offset
            .checked_add(fresh_donor.length)
            .is_none_or(|end| end > plan.target_new_offset)
    {
        return Err(pre_move_error_with_shrink_recovery(
            donor_letter,
            shrink_before,
            anyhow!(
                "{}",
                tr!("收缩后左侧空间与计划不一致，已中止（未移动目标数据）")
            ),
        ));
    }

    journal(
        data_partition,
        &format!(
            "MOVE-LEFT start target_off={} len={} delta={}",
            fresh_target.offset, fresh_target.length, plan.delta
        ),
    );
    let move_identity =
        lr_core::windows_storage::stable_volume_identity(letter).map_err(|error| {
            pre_move_error_with_shrink_recovery(donor_letter, shrink_before, anyhow!(error))
        })?;
    if move_identity.extent.disk_number != disk
        || move_identity.extent.offset_bytes != fresh_target.offset
        || move_identity.extent.extent_length_bytes != fresh_target.length
    {
        return Err(pre_move_error_with_shrink_recovery(
            donor_letter,
            shrink_before,
            anyhow!("target stable identity changed before raw move"),
        ));
    }
    let move_layout = lr_core::windows_storage::disk_layout_snapshot(disk).map_err(|error| {
        pre_move_error_with_shrink_recovery(donor_letter, shrink_before, anyhow!(error))
    })?;
    let target_handle = unsafe { lock_dismount_volume(letter) }
        .map_err(|error| pre_move_error_with_shrink_recovery(donor_letter, shrink_before, error))?;
    let move_result = unsafe {
        raw_move_left(
            disk,
            fresh_target.offset,
            fresh_target.length,
            plan.delta,
            &move_layout,
        )
    };
    unsafe {
        let _ = CloseHandle(target_handle);
    }
    move_result.map_err(|error| {
        journal(data_partition, &format!("MOVE-LEFT FAILED: {}", error));
        anyhow!(
            "{}",
            tr!(
                "向左搬移目标分区 {}: 数据失败；请保留 journal 并停止写盘：{}",
                letter,
                error
            )
        )
    })?;
    journal(data_partition, "MOVE-LEFT done");

    lr_core::windows_storage::delete_partition_checked(
        disk,
        fresh_target.offset,
        true,
        &move_layout,
    )
    .map_err(|error| {
        anyhow!(
            "{}",
            tr!(
                "目标数据已左移到 offset={}，但删除旧分区表项失败；请按 journal 修复：{}",
                plan.target_new_offset,
                error
            )
        )
    })?;
    let recreate_layout = lr_core::windows_storage::disk_layout_snapshot(disk)?;
    let created = lr_core::windows_storage::create_partition_checked(
        &lr_core::windows_storage::CreatePartitionRequest {
            disk_number: disk,
            offset_bytes: plan.target_new_offset,
            size_bytes: fresh_target.length,
            kind: lr_core::windows_storage::PartitionKind::BasicData,
            file_system: None,
            label: String::new(),
            drive_letter: Some(letter),
            active: style == PartitionStyle::MBR && fresh_target.mbr_active,
            preserve_gpt_metadata: fresh_target.gpt_metadata.clone(),
        },
        &recreate_layout,
    )
    .map_err(|error| {
        anyhow!(
            "{}",
            tr!(
                "目标数据已左移到 offset={}，但重建分区表项失败；请按 journal 修复：{}",
                plan.target_new_offset,
                error
            )
        )
    })?;
    reject_and_cleanup_mismatched_recreate(
        disk,
        created,
        plan.target_new_offset,
        fresh_target.length,
        data_partition,
    )?;

    let message =
        DiskManager::expand_partition_lossless_checked(letter, target_size_mb, expected_target)
            .map_err(|error| {
                anyhow!(
                    "{}",
                    tr!(
                "目标分区已成功左移，但最后扩展 {}: 失败；当前表项和数据仍可访问，可重试扩展：{}",
                letter,
                error
            )
                )
            })?;
    journal(data_partition, "DONE-LEFT");
    Ok(tr!("已从左侧分区 {}: 转移空间并{}", donor_letter, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_part(number: u32, offset: u64, length: u64) -> PartEntry {
        PartEntry {
            number,
            offset,
            length,
            is_special: false,
            mbr_type: None,
            mbr_active: false,
            gpt_metadata: None,
        }
    }

    fn expected_left_config() -> crate::core::config::ExpandConfig {
        crate::core::config::ExpandConfig {
            session_id: "0123456789abcdef0123456789abcdef".to_owned(),
            expected_disk_number: 2,
            expected_disk_size_bytes: 1_000 * MIB,
            expected_partition_number: 4,
            expected_partition_offset_bytes: 401 * MIB,
            expected_partition_size_bytes: 200 * MIB,
            expected_donor_partition_number: 3,
            expected_donor_offset_bytes: MIB,
            expected_donor_size_bytes: 400 * MIB,
            ..Default::default()
        }
    }

    fn sector_geometry(
        logical: u32,
        physical: u32,
    ) -> lr_core::windows_storage::DiskSectorGeometry {
        lr_core::windows_storage::DiskSectorGeometry {
            logical_sector_bytes: logical,
            physical_sector_bytes: physical,
            sector_alignment_offset_bytes: 0,
        }
    }

    fn stable_extent(offset: u64, length: u64) -> lr_core::windows_storage::StableVolumeIdentity {
        lr_core::windows_storage::StableVolumeIdentity {
            extent: lr_core::windows_storage::VolumeIdentity {
                disk_number: 2,
                offset_bytes: offset,
                extent_length_bytes: length,
            },
            disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [7; 16] },
            partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                partition_id: [9; 16],
            },
            device_id_hash: Some([11; 32]),
        }
    }

    #[test]
    fn left_transfer_shrinks_adjacent_donor_and_keeps_original_target_end() {
        let plan = plan_left_transfer(MIB, 200 * MIB, 201 * MIB, 80 * MIB, 140 * MIB)
            .expect("valid adjacent transfer");
        assert_eq!(plan.delta, 60 * MIB);
        assert_eq!(plan.donor_shrink_by, 60 * MIB);
        assert_eq!(plan.donor_length_after_shrink, 140 * MIB);
        assert_eq!(plan.target_new_offset, 141 * MIB);
        assert_eq!(plan.target_new_offset + 140 * MIB, 281 * MIB);
    }

    #[test]
    fn right_transfer_keeps_preexisting_trailing_free_space() {
        let shrink = plan_right_donor_shrink(
            100 * MIB,
            125 * MIB,
            0,
            150 * MIB,
            25 * MIB,
            50 * MIB,
            125 * MIB,
        )
        .unwrap();
        assert_eq!(shrink, 25 * MIB);
    }

    #[test]
    fn legacy_right_expansion_can_still_use_trailing_free_space() {
        let shrink =
            plan_right_donor_shrink(100 * MIB, 125 * MIB, 0, 150 * MIB, 25 * MIB, 50 * MIB, 0)
                .unwrap();
        assert_eq!(shrink, 0);
    }

    #[test]
    fn exact_right_transfer_rejects_an_inconsistent_pair_total() {
        assert!(plan_right_donor_shrink(
            100 * MIB,
            125 * MIB,
            0,
            150 * MIB,
            25 * MIB,
            50 * MIB,
            120 * MIB,
        )
        .is_err());
    }

    #[test]
    fn left_transfer_consumes_existing_gap_before_shrinking_donor() {
        let plan = plan_left_transfer(MIB, 100 * MIB, 121 * MIB, 80 * MIB, 130 * MIB)
            .expect("valid transfer with a gap");
        assert_eq!(plan.gap_before_target, 20 * MIB);
        assert_eq!(plan.donor_shrink_by, 30 * MIB);
        assert_eq!(plan.target_new_offset, 71 * MIB);
    }

    #[test]
    fn left_transfer_rejects_overlap_but_accepts_legal_non_mib_geometry() {
        assert!(plan_left_transfer(MIB, 100 * MIB, 90 * MIB, 80 * MIB, 100 * MIB).is_err());
        let sector = 4096;
        let donor_offset = MIB + sector;
        let donor_length = 100 * MIB + sector;
        let target_offset = donor_offset + donor_length;
        let target_length = 80 * MIB + sector;
        let requested = target_length + 20 * MIB;
        let plan = plan_left_transfer(
            donor_offset,
            donor_length,
            target_offset,
            target_length,
            requested,
        )
        .expect("sector-aligned non-MiB layout must not be rejected");
        assert_eq!(plan.delta, 20 * MIB);
        assert_ne!(donor_offset % MIB, 0);
    }

    #[test]
    fn raw_io_accepts_non_mib_512e_and_4kn_geometry() {
        let plan = plan_raw_move_io(
            sector_geometry(512, 4096),
            2 * 1024 * MIB,
            MIB + 512,
            100 * MIB + 512,
            8 * MIB + 512,
            RawMoveDirection::Right,
        )
        .expect("legal 512e geometry");
        assert_eq!(plan.physical_sector_bytes, 4096);

        plan_raw_move_io(
            sector_geometry(4096, 4096),
            2 * 1024 * MIB,
            64 * MIB + 4096,
            100 * MIB + 4096,
            8 * MIB + 4096,
            RawMoveDirection::Left,
        )
        .expect("legal 4Kn geometry");
    }

    #[test]
    fn raw_io_rejects_only_real_device_constraint_and_range_violations() {
        assert!(plan_raw_move_io(
            sector_geometry(4096, 4096),
            1024 * MIB,
            MIB + 1,
            100 * MIB,
            8 * MIB,
            RawMoveDirection::Right,
        )
        .is_err());
        assert!(plan_raw_move_io(
            sector_geometry(512, 4096),
            128 * MIB,
            120 * MIB,
            16 * MIB,
            8 * MIB,
            RawMoveDirection::Right,
        )
        .is_err());
        assert!(plan_raw_move_io(
            lr_core::windows_storage::DiskSectorGeometry {
                logical_sector_bytes: 4096,
                physical_sector_bytes: 4096,
                sector_alignment_offset_bytes: 512,
            },
            1024 * MIB,
            MIB,
            100 * MIB,
            8 * MIB,
            RawMoveDirection::Right,
        )
        .is_err());
    }

    #[test]
    fn raw_io_buffer_address_is_physically_aligned() {
        let mut buffer = AlignedIoBuffer::new(64 * 1024, 4096).unwrap();
        let slice = buffer.as_mut_slice(4096).unwrap();
        assert_eq!((slice.as_ptr() as usize) % 4096, 0);
    }

    #[test]
    fn shrink_recovery_observation_uses_actual_tail_change_only() {
        let before = stable_extent(MIB + 512, 100 * MIB + 512);
        assert_eq!(observed_stable_shrink_bytes(before, before).unwrap(), None);
        let shrunk = lr_core::windows_storage::StableVolumeIdentity {
            extent: lr_core::windows_storage::VolumeIdentity {
                extent_length_bytes: before.extent.extent_length_bytes - (8 * MIB + 4096),
                ..before.extent
            },
            ..before
        };
        assert_eq!(
            observed_stable_shrink_bytes(before, shrunk).unwrap(),
            Some(8 * MIB + 4096)
        );
        let rebound = lr_core::windows_storage::StableVolumeIdentity {
            partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                partition_id: [3; 16],
            },
            ..shrunk
        };
        assert!(observed_stable_shrink_bytes(before, rebound).is_err());
        let grown = lr_core::windows_storage::StableVolumeIdentity {
            extent: lr_core::windows_storage::VolumeIdentity {
                extent_length_bytes: before.extent.extent_length_bytes + 4096,
                ..before.extent
            },
            ..before
        };
        assert!(observed_stable_shrink_bytes(before, grown).is_err());
    }

    #[test]
    fn recreated_partition_must_exactly_match_raw_file_system_range() {
        let raw_offset = MIB + 4096;
        let raw_length = 100 * MIB + 4096;
        assert!(recreated_partition_matches_raw_range(
            lr_core::windows_storage::CreatedPartition {
                offset_bytes: raw_offset,
                size_bytes: raw_length,
            },
            raw_offset,
            raw_length,
        ));
        assert!(!recreated_partition_matches_raw_range(
            lr_core::windows_storage::CreatedPartition {
                offset_bytes: raw_offset - 4096,
                size_bytes: raw_length + 8192,
            },
            raw_offset,
            raw_length,
        ));
    }

    #[test]
    fn left_transfer_identity_requires_exact_adjacent_geometry() {
        let config = expected_left_config();
        let target = test_part(4, 401 * MIB, 200 * MIB);
        let donor = test_part(3, MIB, 400 * MIB);
        validate_left_transfer_identity(&config, 2, 1_000 * MIB, &target, &donor)
            .expect("matching identity");

        let mut changed_target = target.clone();
        changed_target.offset += MIB;
        assert!(
            validate_left_transfer_identity(&config, 2, 1_000 * MIB, &changed_target, &donor)
                .is_err()
        );

        let mut missing_identity = config;
        missing_identity.expected_donor_size_bytes = 0;
        assert!(validate_left_transfer_identity(
            &missing_identity,
            2,
            1_000 * MIB,
            &target,
            &donor
        )
        .is_err());
    }
}
