//! 无损扩容 Case 2：块级分区移动（仅 PE 内执行）。
//!
//! 当 C 盘后方紧邻的不是未分配空间、而是一个**基础数据分区**(如 D:)时，diskpart `extend`
//! 无法把空间并入 C。本模块通过「把后方分区整体向右搬移、在 C 之后腾出未分配空间、再 extend C」
//! 实现真正的无损扩大 C 盘。
//!
//! ## 算法（C 紧跟一个可移动分区 N，N 之后是未分配尾部）
//! 设 C=[c_off, c_off+c_len)，与 C 之间已有未分配 adj；N=[n_off, n_off+n_len)；N 之后空闲 free。
//! 目标把 C 扩大到 target → 需要在 C 之后腾出 delta = target - c_len 的间隙。
//!   - 需要把 N 右移 shift = delta - adj；
//!   - 若 shift > free，先把 N 的文件系统 shrink 掉 (shift - free)，使尾部空出到 shift；
//!   - 右移 N（重叠安全：从高地址向低地址倒序拷贝原始扇区）；
//!   - diskpart 删除旧 N 表项、在新偏移按原大小重建、还原盘符；
//!   - diskpart 把 C extend 到 target。
//!
//! ## 安全防呆（任一不满足直接安全失败，不触碰磁盘）
//! - N 必须是 C 之后**紧邻**的、有盘符的**基础数据分区**(非 ESP/MSR/恢复/系统)；
//! - C/N 的偏移、长度、adj、delta、shift 均须 1 MiB 对齐（绝大多数真实分区如此）；
//! - 搬移前锁定并卸载 N 卷；搬移采用倒序重叠安全拷贝；
//! - 重建分区表项交给 diskpart（避免手改 GPT CRC / MBR 出错）；
//! - 全程写 journal 便于诊断；移动数据期间断电会损坏 N（与所有分区工具同理，需提示勿断电）。
//!
//! ⚠️ 本路径会搬移用户数据，必须先在虚拟机/废盘充分验证后再用于真机。

#![cfg(windows)]

use anyhow::{anyhow, bail, Result};
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, WriteFile, FILE_BEGIN,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, PARTITION_STYLE_GPT,
    PARTITION_STYLE_MBR,
};
use windows::Win32::System::IO::DeviceIoControl;

use crate::core::disk::{DiskManager, PartitionStyle};
use crate::tr;
use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

const MIB: u64 = 1024 * 1024;
const GENERIC_RW: u32 = 0x8000_0000 | 0x4000_0000; // GENERIC_READ | GENERIC_WRITE
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
const COPY_CHUNK: u64 = 4 * MIB;

/// GPT 基础数据分区类型 GUID（小端字节序，与 PARTITION_INFORMATION_GPT 一致）。
const ESP_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const MSR_GUID: [u8; 16] = [
    0x16, 0xe3, 0xc9, 0xe3, 0x5c, 0x0b, 0xb8, 0x4d, 0x81, 0x7d, 0xf9, 0x2d, 0xf0, 0x02, 0x15, 0xae,
];
const RECOVERY_GUID: [u8; 16] = [
    0xa4, 0xbb, 0x94, 0xde, 0xd1, 0x06, 0x40, 0x4d, 0xa1, 0x6a, 0xbf, 0xd5, 0x01, 0x79, 0xd6, 0xac,
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

/// 单个分区的关键几何信息。
#[derive(Debug, Clone)]
struct PartEntry {
    number: u32,
    offset: u64,
    length: u64,
    is_special: bool, // ESP / MSR / 恢复 等不可随意移动的分区
}

fn diskpart_path() -> String {
    let builtin = get_bin_dir().join("diskpart").join("diskpart.exe");
    if builtin.exists() {
        builtin.to_string_lossy().to_string()
    } else {
        "diskpart.exe".to_string()
    }
}

/// 运行一段 diskpart 脚本，返回标准输出（GBK→UTF8）。
fn run_diskpart(script: &str) -> Result<String> {
    let temp_dir = crate::core::system_utils::get_temp_directory();
    std::fs::create_dir_all(&temp_dir).ok();
    let script_path = temp_dir.join("lr_expand_move.txt");
    std::fs::write(&script_path, script)?;
    let output = new_command(&diskpart_path())
        .args(["/s", script_path.to_str().unwrap()])
        .output()?;
    let _ = std::fs::remove_file(&script_path);
    Ok(gbk_to_utf8(&output.stdout))
}

fn diskpart_ok(text: &str) -> bool {
    let l = text.to_lowercase();
    let ok = l.contains("成功") || l.contains("successfully");
    let err = l.contains("error") || l.contains("错误") || l.contains("失败") || l.contains("failed");
    ok && !err
}

/// 读取卷所在物理磁盘号与起始偏移、长度（字节）。
unsafe fn volume_disk_and_offset(letter: char) -> Option<(u32, u64, u64)> {
    let path = format!("\\\\.\\{}:", letter);
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

    #[repr(C)]
    struct DiskExtent {
        disk_number: u32,
        starting_offset: i64,
        extent_length: i64,
    }
    #[repr(C)]
    struct VolumeDiskExtents {
        number_of_disk_extents: u32,
        extents: [DiskExtent; 1],
    }

    let mut buffer = [0u8; 256];
    let mut returned: u32 = 0;
    let res = DeviceIoControl(
        handle,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        None,
        0,
        Some(buffer.as_mut_ptr() as *mut _),
        buffer.len() as u32,
        Some(&mut returned),
        None,
    );
    let _ = CloseHandle(handle);
    if res.is_err() {
        return None;
    }
    let ext = &*(buffer.as_ptr() as *const VolumeDiskExtents);
    if ext.number_of_disk_extents != 1 {
        // 跨多个磁盘范围（跨盘卷）不支持移动
        return None;
    }
    Some((
        ext.extents[0].disk_number,
        ext.extents[0].starting_offset as u64,
        ext.extents[0].extent_length as u64,
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

    let mut buffer = vec![0u8; 65536];
    let mut returned: u32 = 0;
    let layout_ok = DeviceIoControl(
        handle,
        IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
        None,
        0,
        Some(buffer.as_mut_ptr() as *mut _),
        buffer.len() as u32,
        Some(&mut returned),
        None,
    );
    let _ = CloseHandle(handle);
    if layout_ok.is_err() || returned < std::mem::size_of::<DriveLayoutInfoExHeader>() as u32 {
        return None;
    }

    let header = &*(buffer.as_ptr() as *const DriveLayoutInfoExHeader);
    let style = if header.partition_style == PARTITION_STYLE_MBR.0 as u32 {
        PartitionStyle::MBR
    } else if header.partition_style == PARTITION_STYLE_GPT.0 as u32 {
        PartitionStyle::GPT
    } else {
        PartitionStyle::Unknown
    };

    // PARTITION_INFORMATION_EX 固定 144 字节；头部 GPT=48 / MBR=16。
    let entry_size = 144usize;
    let header_size = if style == PartitionStyle::GPT { 48 } else { 16 };

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
        let is_special = if style == PartitionStyle::GPT {
            let mut g = [0u8; 16];
            g.copy_from_slice(&d[32..48]);
            g == ESP_GUID || g == MSR_GUID || g == RECOVERY_GUID
        } else {
            // MBR：仅类型 0x07(NTFS/IFS) / 0x0B / 0x0C(FAT32) 视为普通数据，其余保守地当作特殊不移动。
            let t = d[32];
            !(t == 0x07 || t == 0x0b || t == 0x0c)
        };
        parts.push(PartEntry {
            number,
            offset: starting as u64,
            length: length as u64,
            is_special,
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
    if DeviceIoControl(handle, FSCTL_LOCK_VOLUME, None, 0, None, 0, Some(&mut returned), None).is_err()
    {
        let _ = CloseHandle(handle);
        bail!("{}", tr!("锁定卷 {}: 失败（可能有句柄占用）", letter));
    }
    if DeviceIoControl(handle, FSCTL_DISMOUNT_VOLUME, None, 0, None, 0, Some(&mut returned), None)
        .is_err()
    {
        let _ = CloseHandle(handle);
        bail!("{}", tr!("卸载卷 {}: 失败", letter));
    }
    Ok(handle)
}

/// 在物理磁盘上把 [src, src+len) 整块向右搬移 delta 字节（重叠安全：倒序拷贝）。
unsafe fn raw_move_right(disk_number: u32, src: u64, len: u64, delta: u64) -> Result<()> {
    let path = format!("\\\\.\\PhysicalDrive{}", disk_number);
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
    .map_err(|e| anyhow!("{}", tr!("打开物理磁盘 {} 失败: {}", disk_number, e)))?;
    if handle == INVALID_HANDLE_VALUE {
        bail!("{}", tr!("打开物理磁盘 {} 得到无效句柄", disk_number));
    }

    let result = (|| -> Result<()> {
        let mut buf = vec![0u8; COPY_CHUNK as usize];
        let mut pos = len; // 已处理到区域内的字节位置（从尾部往头部）
        while pos > 0 {
            let this = COPY_CHUNK.min(pos);
            let rel = pos - this;
            let read_at = (src + rel) as i64;
            let write_at = (src + delta + rel) as i64;

            // 读
            seek(handle, read_at)?;
            read_exact(handle, &mut buf[..this as usize])?;
            // 写
            seek(handle, write_at)?;
            write_exact(handle, &buf[..this as usize])?;

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

unsafe fn seek(handle: HANDLE, offset: i64) -> Result<()> {
    SetFilePointerEx(handle, offset, None, FILE_BEGIN)
        .map_err(|e| anyhow!("{}", tr!("定位到 {} 失败: {}", offset, e)))
}

unsafe fn read_exact(handle: HANDLE, buf: &mut [u8]) -> Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        let mut read: u32 = 0;
        ReadFile(handle, Some(&mut buf[done..]), Some(&mut read), None)
            .map_err(|e| anyhow!("{}", tr!("读盘失败: {}", e)))?;
        if read == 0 {
            bail!("{}", tr!("读盘返回 0 字节（已读 {}/{}）", done, buf.len()));
        }
        done += read as usize;
    }
    Ok(())
}

unsafe fn write_exact(handle: HANDLE, buf: &[u8]) -> Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        let mut written: u32 = 0;
        WriteFile(handle, Some(&buf[done..]), Some(&mut written), None)
            .map_err(|e| anyhow!("{}", tr!("写盘失败: {}", e)))?;
        if written == 0 {
            bail!("{}", tr!("写盘返回 0 字节（已写 {}/{}）", done, buf.len()));
        }
        done += written as usize;
    }
    Ok(())
}

fn aligned(v: u64) -> bool {
    v % MIB == 0
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

/// 写一行 journal 便于失败诊断（best-effort）。
fn journal(data_partition: &str, line: &str) {
    let dir = format!("{}\\LetRecovery_Data", data_partition);
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{}\\expand_move.journal", dir);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
    }
}

/// 编排：把分区 `letter` 无损扩大到 `target_size_mb`（0=尽量并入相邻未分配空间）。
///
/// 优先 Case 1（diskpart extend 并入相邻未分配空间）；不足时尝试 Case 2（移动紧邻的基础数据分区）。
/// `data_partition` 仅用于写 journal。
pub fn expand_c_drive(letter: char, target_size_mb: u64, data_partition: &str) -> Result<String> {
    // 0=尽量扩到相邻未分配空间最大 → 直接 Case 1。
    if target_size_mb == 0 {
        return DiskManager::expand_partition_lossless(letter, 0).map_err(|e| anyhow!(e));
    }

    let (disk, c_off, c_len) = unsafe { volume_disk_and_offset(letter) }
        .ok_or_else(|| anyhow!("{}", tr!("无法定位分区 {}: 所在磁盘/偏移", letter)))?;
    let target_bytes = target_size_mb * MIB;
    if target_bytes <= c_len {
        return Ok(tr!("分区 {}: 当前已达到或超过目标大小，无需扩容", letter));
    }
    let delta = target_bytes - c_len;

    let (style, disk_size, parts) =
        unsafe { read_disk_layout(disk) }.ok_or_else(|| anyhow!("{}", tr!("读取磁盘 {} 布局失败", disk)))?;
    if style == PartitionStyle::Unknown {
        bail!("{}", tr!("磁盘 {} 分区表类型未知，拒绝操作", disk));
    }

    // 找到 C 之后紧邻的分区 N（offset 最小且 > c_off）。
    let c_end = c_off + c_len;
    let next = parts.iter().filter(|p| p.offset >= c_end).min_by_key(|p| p.offset);
    let adj_unalloc = match next {
        Some(n) => n.offset.saturating_sub(c_end),
        None => disk_size.saturating_sub(c_end),
    };

    // 相邻未分配空间已够 → Case 1。
    if delta <= adj_unalloc {
        return DiskManager::expand_partition_lossless(letter, target_size_mb).map_err(|e| anyhow!(e));
    }

    // 否则需要移动后方分区（Case 2）。
    let n = next.ok_or_else(|| {
        anyhow!("{}", tr!("C 盘后方空间不足且无可移动分区（相邻未分配仅 {} MiB）", adj_unalloc / MIB))
    })?;

    // ===== 防呆校验（任一不满足，安全失败，不触碰磁盘）=====
    if n.is_special {
        bail!("{}", tr!("C 盘后方分区是系统/ESP/MSR/恢复等特殊分区，为安全起见拒绝移动"));
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
    // N 右移 shift 后需要的尾部空间：若 free_after_n 不足，先 shrink N。
    let shrink_by = shift.saturating_sub(free_after_n);

    // 对齐校验（绝大多数真实分区 1 MiB 对齐；不对齐则拒绝以保证 diskpart offset/size 精确）。
    for (name, v) in [
        ("C 偏移", c_off),
        ("C 长度", c_len),
        ("N 偏移", n.offset),
        ("N 长度", n.length),
        ("相邻未分配", adj_unalloc),
        ("delta", delta),
        ("shift", shift),
        ("shrink", shrink_by),
        ("N 尾部空闲", free_after_n),
    ] {
        if !aligned(v) {
            bail!("{}", tr!("几何未按 1 MiB 对齐（{}={} 字节），为保证精确重建拒绝移动", name, v));
        }
    }

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
        disk, n.number, n_letter, shift / MIB, shrink_by / MIB
    );

    // ===== Step A：必要时 shrink N 文件系统 =====
    let mut n_len_now = n.length;
    if shrink_by > 0 {
        journal(data_partition, &format!("SHRINK {}: by {} MiB", n_letter, shrink_by / MIB));
        let script = format!("select volume {}\r\nshrink desired={}\r\n", n_letter, shrink_by / MIB);
        let out = run_diskpart(&script)?;
        log::info!("[EXPAND-MOVE] shrink 输出: {}", out);
        if !diskpart_ok(&out) {
            bail!("{}", tr!("收缩后方分区 {}: 失败，未做任何移动。输出：{}", n_letter, out));
        }
        // 重新读取布局，确认 N 偏移未变、长度已减小且仍 1 MiB 对齐。
        let (_s2, _ds2, parts2) =
            unsafe { read_disk_layout(disk) }.ok_or_else(|| anyhow!("{}", tr!("shrink 后重读磁盘布局失败")))?;
        let n2 = parts2
            .iter()
            .find(|p| p.number == n.number && p.offset == n.offset)
            .ok_or_else(|| anyhow!("{}", tr!("shrink 后未找到原分区，已中止（未移动数据）")))?;
        if !aligned(n2.length) {
            bail!("{}", tr!("shrink 后分区长度非 1 MiB 对齐，已中止（未移动数据）"));
        }
        n_len_now = n2.length;
        // 再次确认右移后能放下：n.offset+shift+n_len_now <= after_n
        if n.offset + shift + n_len_now > after_n {
            bail!("{}", tr!("shrink 后空间仍不足，已中止（未移动数据）"));
        }
    }

    // ===== Step B：锁定/卸载 N，倒序重叠安全搬移 =====
    journal(data_partition, &format!("MOVE start n_off={} len={} shift={}", n.offset, n_len_now, shift));
    let vol_handle = unsafe { lock_dismount_volume(n_letter) }?;
    let move_res = unsafe { raw_move_right(disk, n.offset, n_len_now, shift) };
    unsafe {
        let _ = CloseHandle(vol_handle);
    }
    move_res.map_err(|e| {
        journal(data_partition, &format!("MOVE FAILED: {}", e));
        anyhow!("{}", tr!("搬移分区数据失败（分区 {} 可能已损坏，请用 journal 诊断）：{}", n_letter, e))
    })?;
    journal(data_partition, "MOVE done");

    // ===== Step C：diskpart 删除旧表项、按原大小在新偏移重建、还原盘符 =====
    let new_off = n.offset + shift;
    let mut recreate = format!(
        "select disk {}\r\nselect partition {}\r\ndelete partition override\r\ncreate partition primary offset={} size={}\r\n",
        disk,
        n.number,
        new_off / 1024,      // KB
        n_len_now / MIB,     // MB
    );
    if style == PartitionStyle::MBR {
        recreate.push_str("set id=07\r\n"); // NTFS/IFS
    }
    recreate.push_str(&format!("assign letter={}\r\n", n_letter));
    journal(data_partition, &format!("RECREATE off={} size={} letter={}", new_off, n_len_now, n_letter));
    let out = run_diskpart(&recreate)?;
    log::info!("[EXPAND-MOVE] recreate 输出: {}", out);
    if !diskpart_ok(&out) {
        bail!(
            "{}",
            tr!("搬移已完成但重建分区表项失败（分区 {} 数据在新位置 offset={} 但表项未建好，请据 journal 手工修复）。输出：{}",
            n_letter, new_off, out)
        );
    }

    // ===== Step D：把 C extend 到目标 =====
    journal(data_partition, "EXTEND C");
    let msg = DiskManager::expand_partition_lossless(letter, target_size_mb).map_err(|e| {
        anyhow!("{}", tr!("分区已成功移动，但最后扩展 C 失败：{}（可重试一键扩容，此时已是相邻未分配空间）", e))
    })?;
    journal(data_partition, "DONE");
    Ok(tr!("已移动后方分区 {} 并{}", n_letter, msg))
}
