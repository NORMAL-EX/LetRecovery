//! 分区对拷模块
//!
//! 提供分区级别的文件复制功能，支持断点续传。
//! 使用 WinAPI 实现，不依赖外部工具。

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE, HANDLE},
    Win32::Storage::FileSystem::{
        CreateFileW, FindClose, FindFirstFileW, FindNextFileW, GetDiskFreeSpaceExW,
        GetDriveTypeW, GetFileAttributesW, GetVolumeInformationW, SetFileAttributesW,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
        WIN32_FIND_DATAW,
    },
};

/// 驱动器类型常量
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_CDROM: u32 = 5;

/// 标记文件名
const COPY_MARKER_FILENAME: &str = ".letrecovery_partition_copy_marker";

/// 分区复制信息
#[derive(Debug, Clone)]
pub struct CopyablePartition {
    /// 盘符（如 "D:"）
    pub letter: String,
    /// 卷标
    pub label: String,
    /// 总大小（MB）
    pub total_size_mb: u64,
    /// 已用空间（MB）
    pub used_size_mb: u64,
    /// 可用空间（MB）
    pub free_size_mb: u64,
    /// 是否有系统
    pub has_system: bool,
    /// 是否为可移动设备
    pub is_removable: bool,
}

/// 对拷标记文件内容
#[derive(Debug, Clone, Default)]
pub struct CopyMarker {
    /// 源分区
    pub source_partition: String,
    /// 创建时间
    pub created_time: String,
    /// 已复制的文件列表（相对路径）
    pub copied_files: HashSet<String>,
}

/// 对拷进度信息
#[derive(Debug, Clone)]
pub struct CopyProgress {
    /// 当前正在复制的文件
    pub current_file: String,
    /// 已复制文件数量
    pub copied_count: usize,
    /// 总文件数量
    pub total_count: usize,
    /// 是否完成
    pub completed: bool,
    /// 错误信息
    pub error: Option<String>,
    /// 跳过的文件数量（已存在于标记中）
    pub skipped_count: usize,
    /// 失败的文件数量
    pub failed_count: usize,
    /// 失败的文件列表
    pub failed_files: Vec<String>,
}

impl Default for CopyProgress {
    fn default() -> Self {
        Self {
            current_file: String::new(),
            copied_count: 0,
            total_count: 0,
            completed: false,
            error: None,
            skipped_count: 0,
            failed_count: 0,
            failed_files: Vec::new(),
        }
    }
}

/// 获取驱动器类型
#[cfg(windows)]
fn get_drive_type(path: &str) -> u32 {
    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) }
}

#[cfg(not(windows))]
fn get_drive_type(_path: &str) -> u32 {
    0
}

/// 检查分区是否包含 Windows 系统
fn check_has_windows(letter: &str) -> bool {
    let system32_path = format!("{}\\Windows\\System32", letter);
    Path::new(&system32_path).exists()
}

/// 获取分区信息
#[cfg(windows)]
fn get_partition_info(drive: &str) -> Option<CopyablePartition> {
    let path = format!("{}\\", drive);
    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // 获取驱动器类型
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };

    // 排除光驱
    if drive_type == DRIVE_CDROM {
        return None;
    }

    // 只接受固定磁盘和可移动设备（USB）
    if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
        return None;
    }

    // 获取卷标
    let mut volume_name = [0u16; 261];
    unsafe {
        let result = GetVolumeInformationW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut volume_name),
            None,
            None,
            None,
            None,
        );

        if result.is_err() {
            return None;
        }
    }

    let label = String::from_utf16_lossy(&volume_name)
        .trim_end_matches('\0')
        .to_string();

    // 获取磁盘空间
    let mut free_bytes_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free_bytes: u64 = 0;

    unsafe {
        if GetDiskFreeSpaceExW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut free_bytes_available as *mut u64),
            Some(&mut total_bytes as *mut u64),
            Some(&mut total_free_bytes as *mut u64),
        )
        .is_err()
        {
            return None;
        }
    }

    let total_size_mb = total_bytes / 1024 / 1024;
    let free_size_mb = free_bytes_available / 1024 / 1024;
    let used_size_mb = total_size_mb.saturating_sub(free_size_mb);

    Some(CopyablePartition {
        letter: drive.to_string(),
        label,
        total_size_mb,
        used_size_mb,
        free_size_mb,
        has_system: check_has_windows(drive),
        is_removable: drive_type == DRIVE_REMOVABLE,
    })
}

#[cfg(not(windows))]
fn get_partition_info(_drive: &str) -> Option<CopyablePartition> {
    None
}

/// 获取所有可用于对拷的分区列表
pub fn get_copyable_partitions() -> Vec<CopyablePartition> {
    let mut partitions = Vec::new();
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());

    for letter in b'A'..=b'Z' {
        let drive = format!("{}:", letter as char);
        let drive_path = format!("{}\\", drive);

        // 检查分区是否存在
        if !Path::new(&drive_path).exists() {
            continue;
        }

        // 排除当前运行的系统分区（PE 环境下通常是 X:）
        if drive.to_uppercase() == system_drive.to_uppercase() {
            continue;
        }

        // 检查是否为光驱
        let drive_type = get_drive_type(&drive_path);
        if drive_type == DRIVE_CDROM {
            continue;
        }

        if let Some(info) = get_partition_info(&drive) {
            partitions.push(info);
        }
    }

    partitions
}

/// 读取对拷标记文件
pub fn read_copy_marker(target_partition: &str) -> Option<CopyMarker> {
    let marker_path = format!("{}\\{}", target_partition, COPY_MARKER_FILENAME);

    if !Path::new(&marker_path).exists() {
        return None;
    }

    let file = File::open(&marker_path).ok()?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // 第一行：源分区
    let source_partition = lines.next()?.ok()?.trim().to_string();
    // 第二行：创建时间
    let created_time = lines.next()?.ok()?.trim().to_string();

    // 其余行：已复制的文件
    let mut copied_files = HashSet::new();
    for line in lines {
        if let Ok(file_path) = line {
            let trimmed = file_path.trim();
            if !trimmed.is_empty() {
                copied_files.insert(trimmed.to_string());
            }
        }
    }

    Some(CopyMarker {
        source_partition,
        created_time,
        copied_files,
    })
}

/// 写入对拷标记文件
fn write_copy_marker(target_partition: &str, marker: &CopyMarker) -> std::io::Result<()> {
    let marker_path = format!("{}\\{}", target_partition, COPY_MARKER_FILENAME);
    let file = File::create(&marker_path)?;
    let mut writer = BufWriter::new(file);

    writeln!(writer, "{}", marker.source_partition)?;
    writeln!(writer, "{}", marker.created_time)?;

    for file_path in &marker.copied_files {
        writeln!(writer, "{}", file_path)?;
    }

    writer.flush()?;

    // 设置为隐藏文件
    #[cfg(windows)]
    {
        let wide_path: Vec<u16> = marker_path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = SetFileAttributesW(PCWSTR(wide_path.as_ptr()), FILE_ATTRIBUTE_HIDDEN);
        }
    }

    Ok(())
}

/// 追加已复制文件到标记文件
fn append_to_marker(target_partition: &str, relative_path: &str) -> std::io::Result<()> {
    let marker_path = format!("{}\\{}", target_partition, COPY_MARKER_FILENAME);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&marker_path)?;

    writeln!(file, "{}", relative_path)?;
    Ok(())
}

/// 删除对拷标记文件
pub fn delete_copy_marker(target_partition: &str) -> std::io::Result<()> {
    let marker_path = format!("{}\\{}", target_partition, COPY_MARKER_FILENAME);
    if Path::new(&marker_path).exists() {
        // 先移除只读/隐藏属性
        #[cfg(windows)]
        {
            let wide_path: Vec<u16> = marker_path.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let _ = SetFileAttributesW(PCWSTR(wide_path.as_ptr()), FILE_ATTRIBUTE_NORMAL);
            }
        }
        fs::remove_file(&marker_path)?;
    }
    Ok(())
}

/// 检查是否可以继续对拷（目标分区有标记文件且源分区匹配）
pub fn can_resume_copy(source_partition: &str, target_partition: &str) -> bool {
    if let Some(marker) = read_copy_marker(target_partition) {
        return marker.source_partition.eq_ignore_ascii_case(source_partition);
    }
    false
}

/// 递归收集所有文件（使用 WinAPI）
#[cfg(windows)]
fn collect_all_files(root_path: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut dirs_to_process = vec![PathBuf::from(root_path)];

    while let Some(current_dir) = dirs_to_process.pop() {
        let search_pattern = current_dir.join("*");
        let pattern_str = search_pattern.to_string_lossy();
        let wide_pattern: Vec<u16> = pattern_str.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            let mut find_data: WIN32_FIND_DATAW = std::mem::zeroed();
            
            // FindFirstFileW 返回 Result<HANDLE, Error>
            let handle: HANDLE = match FindFirstFileW(PCWSTR(wide_pattern.as_ptr()), &mut find_data) {
                Ok(h) => h,
                Err(_) => continue,
            };

            // 检查是否为无效句柄
            if handle == INVALID_HANDLE_VALUE {
                continue;
            }

            loop {
                let file_name = String::from_utf16_lossy(
                    &find_data
                        .cFileName
                        .iter()
                        .take_while(|&&c| c != 0)
                        .copied()
                        .collect::<Vec<u16>>(),
                );

                // 跳过 . 和 ..
                if file_name != "." && file_name != ".." {
                    let full_path = current_dir.join(&file_name);

                    // 跳过标记文件
                    if file_name == COPY_MARKER_FILENAME {
                        // 不处理
                    } else if (find_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0 {
                        // 目录：加入待处理队列
                        dirs_to_process.push(full_path);
                    } else {
                        // 文件：加入列表
                        files.push(full_path.to_string_lossy().to_string());
                    }
                }

                // FindNextFileW 需要 HANDLE
                if FindNextFileW(handle, &mut find_data).is_err() {
                    break;
                }
            }

            // FindClose 需要 HANDLE
            let _ = FindClose(handle);
        }
    }

    files
}

#[cfg(not(windows))]
fn collect_all_files(_root_path: &str) -> Vec<String> {
    Vec::new()
}

/// 获取相对路径（从源根目录）
fn get_relative_path(full_path: &str, root_path: &str) -> String {
    let full = Path::new(full_path);
    let root = Path::new(root_path);

    full.strip_prefix(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| full_path.to_string())
}

/// 复制单个文件（使用 WinAPI 保持文件属性和时间戳）
#[cfg(windows)]
fn copy_file_with_attributes(source: &str, target: &str) -> std::io::Result<()> {
    // 确保目标目录存在
    if let Some(parent) = Path::new(target).parent() {
        fs::create_dir_all(parent)?;
    }

    // 获取源文件属性
    let wide_source: Vec<u16> = source.encode_utf16().chain(std::iter::once(0)).collect();
    let source_attrs = unsafe { GetFileAttributesW(PCWSTR(wide_source.as_ptr())) };

    // 使用标准库复制文件内容
    fs::copy(source, target)?;

    // 复制文件属性
    if source_attrs != INVALID_FILE_ATTRIBUTES {
        let wide_target: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = SetFileAttributesW(
                PCWSTR(wide_target.as_ptr()),
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(source_attrs),
            );
        }
    }

    // 复制文件时间戳
    copy_file_times(source, target)?;

    Ok(())
}

#[cfg(not(windows))]
fn copy_file_with_attributes(source: &str, target: &str) -> std::io::Result<()> {
    if let Some(parent) = Path::new(target).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, target)?;
    Ok(())
}

/// 复制文件时间戳
#[cfg(windows)]
fn copy_file_times(source: &str, target: &str) -> std::io::Result<()> {
    use windows::Win32::Storage::FileSystem::{
        GetFileTime, SetFileTime, FILE_FLAG_BACKUP_SEMANTICS,
    };

    let wide_source: Vec<u16> = source.encode_utf16().chain(std::iter::once(0)).collect();
    let wide_target: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        // 打开源文件获取时间
        let source_handle: HANDLE = match CreateFileW(
            PCWSTR(wide_source.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        ) {
            Ok(h) => h,
            Err(_) => return Ok(()), // 忽略错误，继续执行
        };

        if source_handle == INVALID_HANDLE_VALUE {
            return Ok(());
        }

        let mut creation_time: FILETIME = std::mem::zeroed();
        let mut last_access_time: FILETIME = std::mem::zeroed();
        let mut last_write_time: FILETIME = std::mem::zeroed();

        let get_result = GetFileTime(
            source_handle,
            Some(&mut creation_time),
            Some(&mut last_access_time),
            Some(&mut last_write_time),
        );

        windows::Win32::Foundation::CloseHandle(source_handle).ok();

        if get_result.is_err() {
            return Ok(());
        }

        // 打开目标文件设置时间
        let target_handle: HANDLE = match CreateFileW(
            PCWSTR(wide_target.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        ) {
            Ok(h) => h,
            Err(_) => return Ok(()),
        };

        if target_handle == INVALID_HANDLE_VALUE {
            return Ok(());
        }

        let _ = SetFileTime(
            target_handle,
            Some(&creation_time),
            Some(&last_access_time),
            Some(&last_write_time),
        );

        windows::Win32::Foundation::CloseHandle(target_handle).ok();
    }

    Ok(())
}

#[cfg(not(windows))]
fn copy_file_times(_source: &str, _target: &str) -> std::io::Result<()> {
    Ok(())
}

/// 执行分区对拷操作
pub fn execute_partition_copy(
    source_partition: &str,
    target_partition: &str,
    progress_tx: Sender<CopyProgress>,
    is_resume: bool,
) {
    let source_root = format!("{}\\", source_partition);
    let target_root = format!("{}\\", target_partition);

    // 发送初始进度
    let mut progress = CopyProgress::default();
    progress.current_file = "正在收集文件列表...".to_string();
    let _ = progress_tx.send(progress.clone());

    // 收集所有文件
    let all_files = collect_all_files(&source_root);
    progress.total_count = all_files.len();

    // 读取或创建标记文件
    let mut marker = if is_resume {
        read_copy_marker(target_partition).unwrap_or_else(|| CopyMarker {
            source_partition: source_partition.to_string(),
            created_time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            copied_files: HashSet::new(),
        })
    } else {
        CopyMarker {
            source_partition: source_partition.to_string(),
            created_time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            copied_files: HashSet::new(),
        }
    };

    // 写入初始标记文件
    if !is_resume {
        if let Err(e) = write_copy_marker(target_partition, &marker) {
            progress.error = Some(format!("无法创建标记文件: {}", e));
            progress.completed = true;
            let _ = progress_tx.send(progress);
            return;
        }
    }

    // 开始复制
    let mut actual_copied = 0usize;
    for source_file in all_files.iter() {
        let relative_path = get_relative_path(source_file, &source_root);

        // 检查是否已复制
        if marker.copied_files.contains(&relative_path) {
            progress.skipped_count += 1;
            continue;
        }

        // 更新进度
        progress.current_file = relative_path.clone();
        let _ = progress_tx.send(progress.clone());

        // 构建目标路径
        let target_file = format!("{}{}", target_root, relative_path);

        // 复制文件
        match copy_file_with_attributes(source_file, &target_file) {
            Ok(_) => {
                // 记录到标记文件
                marker.copied_files.insert(relative_path.clone());
                if let Err(_) = append_to_marker(target_partition, &relative_path) {
                    // 忽略写入标记失败
                }
                actual_copied += 1;
                progress.copied_count = actual_copied;
            }
            Err(e) => {
                progress.failed_count += 1;
                progress.failed_files.push(format!("{}: {}", relative_path, e));
                // 继续复制其他文件，不中断
            }
        }
    }

    // 复制完成，删除标记文件
    if let Err(e) = delete_copy_marker(target_partition) {
        log::warn!("删除标记文件失败: {}", e);
    }

    // 发送完成进度
    progress.completed = true;
    progress.current_file = "复制完成".to_string();
    let _ = progress_tx.send(progress);
}

/// 检查是否有足够的目标空间
pub fn check_target_space(source_partition: &str, target_partition: &str) -> Result<(), String> {
    let source_info = get_partition_info(source_partition)
        .ok_or_else(|| format!("无法获取源分区 {} 的信息", source_partition))?;

    let target_info = get_partition_info(target_partition)
        .ok_or_else(|| format!("无法获取目标分区 {} 的信息", target_partition))?;

    // 检查目标分区可用空间是否足够容纳源分区的已用空间
    if target_info.free_size_mb < source_info.used_size_mb {
        return Err(format!(
            "目标分区可用空间不足！\n源分区已用: {:.2} GB\n目标分区可用: {:.2} GB",
            source_info.used_size_mb as f64 / 1024.0,
            target_info.free_size_mb as f64 / 1024.0
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marker_file_name() {
        assert_eq!(COPY_MARKER_FILENAME, ".letrecovery_partition_copy_marker");
    }
}
