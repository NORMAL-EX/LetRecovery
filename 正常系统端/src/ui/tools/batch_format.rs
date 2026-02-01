//! 批量格式化模块
//!
//! 提供分区格式化功能，使用系统 format 命令实现

use std::path::Path;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
    },
};

/// 驱动器类型常量
const DRIVE_FIXED: u32 = 3;

/// 可格式化的分区信息
#[derive(Debug, Clone)]
pub struct FormatablePartition {
    /// 盘符（如 "D:"）
    pub letter: String,
    /// 卷标
    pub label: String,
    /// 总大小（MB）
    pub total_size_mb: u64,
    /// 可用空间（MB）
    pub free_size_mb: u64,
    /// 文件系统类型
    pub file_system: String,
    /// 是否为系统盘
    pub is_system: bool,
}

/// 格式化结果
#[derive(Debug, Clone)]
pub struct FormatResult {
    /// 盘符
    pub letter: String,
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: String,
}

/// 批量格式化总结果
#[derive(Debug, Clone)]
pub struct BatchFormatResult {
    /// 成功数量
    pub success_count: usize,
    /// 失败数量
    pub fail_count: usize,
    /// 各分区结果
    pub results: Vec<FormatResult>,
}

/// 获取当前系统盘符
pub fn get_system_drive() -> String {
    std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string())
}

/// 检查是否为PE环境
fn is_pe_environment() -> bool {
    crate::core::system_info::SystemInfo::check_pe_environment()
}

/// 获取所有可格式化的分区列表（排除系统盘）
pub fn get_formatable_partitions() -> Vec<FormatablePartition> {
    let mut partitions = Vec::new();
    let is_pe = is_pe_environment();

    // 获取系统盘
    let system_drive = get_system_drive().to_uppercase();

    for letter in b'A'..=b'Z' {
        let drive_letter = (letter as char).to_string();
        let drive = format!("{}:", drive_letter);
        let drive_path = format!("{}\\", drive);

        // 跳过系统盘
        if drive.to_uppercase() == system_drive {
            continue;
        }

        // PE环境下跳过X:盘
        if is_pe && drive_letter.to_uppercase() == "X" {
            continue;
        }

        // 检查分区是否存在
        if !Path::new(&drive_path).exists() {
            continue;
        }

        // 检查是否为固定磁盘
        if !is_fixed_drive(&drive_path) {
            continue;
        }

        // 获取分区信息
        if let Some(info) = get_partition_info(&drive) {
            partitions.push(info);
        }
    }

    partitions
}

/// 检查是否为固定磁盘
#[cfg(windows)]
fn is_fixed_drive(path: &str) -> bool {
    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let drive_type = GetDriveTypeW(PCWSTR(wide_path.as_ptr()));
        drive_type == DRIVE_FIXED
    }
}

#[cfg(not(windows))]
fn is_fixed_drive(_path: &str) -> bool {
    false
}

/// 获取分区详细信息
#[cfg(windows)]
fn get_partition_info(drive: &str) -> Option<FormatablePartition> {
    let path = format!("{}\\", drive);
    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // 获取卷标和文件系统
    let mut volume_name = [0u16; 261];
    let mut file_system_name = [0u16; 261];

    unsafe {
        let success = GetVolumeInformationW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut volume_name),
            None,
            None,
            None,
            Some(&mut file_system_name),
        );

        if success.is_err() {
            return None;
        }
    }

    let label = String::from_utf16_lossy(&volume_name)
        .trim_end_matches('\0')
        .to_string();
    let file_system = String::from_utf16_lossy(&file_system_name)
        .trim_end_matches('\0')
        .to_string();

    // 获取磁盘空间
    let mut free_bytes_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut _total_free_bytes: u64 = 0;

    unsafe {
        if GetDiskFreeSpaceExW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut free_bytes_available as *mut u64),
            Some(&mut total_bytes as *mut u64),
            Some(&mut _total_free_bytes as *mut u64),
        )
        .is_err()
        {
            return None;
        }
    }

    // 检查是否为系统盘
    let system_drive = get_system_drive().to_uppercase();
    let is_system = drive.to_uppercase() == system_drive;

    Some(FormatablePartition {
        letter: drive.to_string(),
        label,
        total_size_mb: total_bytes / 1024 / 1024,
        free_size_mb: free_bytes_available / 1024 / 1024,
        file_system,
        is_system,
    })
}

#[cfg(not(windows))]
fn get_partition_info(_drive: &str) -> Option<FormatablePartition> {
    None
}

/// 使用 format.com 格式化分区
#[cfg(windows)]
pub fn format_partition(letter: &str, label: &str, file_system: &str) -> Result<(), String> {
    use crate::utils::cmd::create_command;
    use crate::utils::encoding::gbk_to_utf8;
    
    // 确保盘符格式正确
    let drive_letter = letter
        .chars()
        .next()
        .ok_or_else(|| "无效的盘符".to_string())?;

    if !drive_letter.is_ascii_alphabetic() {
        return Err("无效的盘符".to_string());
    }

    let drive = format!("{}:", drive_letter.to_ascii_uppercase());

    // 确定文件系统类型
    let fs = if file_system.is_empty() {
        "NTFS"
    } else {
        file_system
    };
    
    // 卷标处理
    let vol_label = if label.is_empty() { "OS" } else { label };

    log::info!(
        "开始格式化分区: {} (文件系统: {}, 卷标: {})",
        drive,
        fs,
        vol_label
    );

    // 使用系统 format 命令: format D: /FS:NTFS /V:Label /Q /Y
    let cmd_args = format!("format {} /FS:{} /V:{} /Q /Y", drive, fs, vol_label);
    
    log::info!("执行命令: cmd /c {}", cmd_args);

    let output = create_command("cmd")
        .args(["/c", &cmd_args])
        .output()
        .map_err(|e| format!("执行 format 命令失败: {}", e))?;

    let stdout = gbk_to_utf8(&output.stdout);
    let stderr = gbk_to_utf8(&output.stderr);

    log::info!("format 输出:\n{}", stdout);
    if !stderr.is_empty() {
        log::warn!("format 错误输出:\n{}", stderr);
    }

    // 检查执行结果
    let stdout_lower = stdout.to_lowercase();
    let success_indicators = ["格式化完成", "format complete", "已完成", "complete"];
    let has_success_indicator = success_indicators.iter().any(|s| stdout_lower.contains(&s.to_lowercase()));
    
    if output.status.success() || has_success_indicator {
        log::info!("分区 {} 格式化成功", drive);
        Ok(())
    } else {
        let error_msg = if !stderr.is_empty() {
            stderr.trim().to_string()
        } else if stdout.contains("无法") || stdout.contains("错误") || stdout.contains("失败") 
            || stdout.contains("denied") || stdout.contains("error") || stdout.contains("拒绝") {
            stdout.trim().to_string()
        } else {
            format!("格式化失败: {}", stdout.trim())
        };
        
        log::error!("格式化失败: {}", error_msg);
        Err(error_msg)
    }
}

/// 使用 format 命令格式化分区（带进度回调）
#[cfg(windows)]
pub fn format_partition_with_progress<F>(
    letter: &str, 
    label: &str, 
    file_system: &str,
    progress_callback: F,
) -> Result<(), String> 
where
    F: Fn(u8, &str) + Send + 'static,
{
    use crate::utils::cmd::create_command;
    use crate::utils::encoding::gbk_to_utf8;
    
    // 确保盘符格式正确
    let drive_letter = letter
        .chars()
        .next()
        .ok_or_else(|| "无效的盘符".to_string())?;

    if !drive_letter.is_ascii_alphabetic() {
        return Err("无效的盘符".to_string());
    }

    let drive = format!("{}:", drive_letter.to_ascii_uppercase());

    // 确定文件系统类型
    let fs = if file_system.is_empty() {
        "NTFS"
    } else {
        file_system
    };
    let vol_label = if label.is_empty() { "OS" } else { label };

    log::info!(
        "开始格式化分区: {} (文件系统: {}, 卷标: {})",
        drive,
        fs,
        vol_label
    );

    progress_callback(0, &format!("准备格式化 {} ...", drive));

    progress_callback(10, "启动格式化进程...");

    // 使用系统 format 命令
    let cmd_args = format!("format {} /FS:{} /V:{} /Q /Y", drive, fs, vol_label);

    log::info!("执行命令: cmd /c {}", cmd_args);

    progress_callback(20, "正在格式化...");

    let output = create_command("cmd")
        .args(["/c", &cmd_args])
        .output()
        .map_err(|e| format!("执行 format 命令失败: {}", e))?;

    let stdout = gbk_to_utf8(&output.stdout);
    let stderr = gbk_to_utf8(&output.stderr);

    log::info!("format 输出:\n{}", stdout);

    // 检查结果
    let stdout_lower = stdout.to_lowercase();
    let success_indicators = ["格式化完成", "format complete", "已完成", "complete"];
    let has_success_indicator = success_indicators.iter().any(|s| stdout_lower.contains(&s.to_lowercase()));

    if output.status.success() || has_success_indicator {
        progress_callback(100, &format!("分区 {} 格式化完成", drive));
        log::info!("分区 {} 格式化成功", drive);
        Ok(())
    } else {
        let error_msg = if !stderr.is_empty() {
            stderr.trim().to_string()
        } else if stdout.contains("无法") || stdout.contains("错误") || stdout.contains("失败") 
            || stdout.contains("denied") || stdout.contains("error") || stdout.contains("拒绝") {
            stdout.trim().to_string()
        } else {
            format!("格式化失败: {}", stdout.trim())
        };
        log::error!("{}", error_msg);
        Err(error_msg)
    }
}

#[cfg(not(windows))]
pub fn format_partition(_letter: &str, _label: &str, _file_system: &str) -> Result<(), String> {
    Err("仅支持Windows系统".to_string())
}

#[cfg(not(windows))]
pub fn format_partition_with_progress<F>(
    _letter: &str, 
    _label: &str, 
    _file_system: &str,
    _progress_callback: F,
) -> Result<(), String> 
where
    F: Fn(u8, &str) + Send + 'static,
{
    Err("仅支持Windows系统".to_string())
}

/// 批量格式化分区
pub fn batch_format_partitions(
    partitions: &[String],
    label: &str,
    file_system: &str,
) -> BatchFormatResult {
    let mut results = Vec::new();
    let mut success_count = 0;
    let mut fail_count = 0;

    for partition in partitions {
        match format_partition(partition, label, file_system) {
            Ok(_) => {
                results.push(FormatResult {
                    letter: partition.clone(),
                    success: true,
                    message: "格式化成功".to_string(),
                });
                success_count += 1;
            }
            Err(e) => {
                results.push(FormatResult {
                    letter: partition.clone(),
                    success: false,
                    message: e,
                });
                fail_count += 1;
            }
        }
    }

    BatchFormatResult {
        success_count,
        fail_count,
        results,
    }
}

/// 检查 format 命令是否可用
pub fn is_format_api_available() -> bool {
    // 系统自带的 format.com 应该总是存在
    std::path::Path::new(r"C:\Windows\System32\format.com").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_system_drive() {
        let drive = get_system_drive();
        assert!(drive.len() >= 2);
        assert!(drive.ends_with(':'));
    }

    #[test]
    #[cfg(windows)]
    fn test_format_api_available() {
        // 系统 format.com 应该存在
        assert!(is_format_api_available());
    }
}