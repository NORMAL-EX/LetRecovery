use anyhow::Result;
use std::path::Path;
use crate::utils::cmd::create_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;
use crate::core::bitlocker::{BitLockerManager, VolumeStatus};

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Win32::Storage::FileSystem::{
        CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    Win32::System::IO::DeviceIoControl,
    Win32::System::Ioctl::{
        IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_STORAGE_GET_DEVICE_NUMBER,
        PARTITION_STYLE_GPT, PARTITION_STYLE_MBR,
    },
};

// 驱动器类型常量
#[allow(dead_code)]
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
#[allow(dead_code)]
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
#[allow(dead_code)]
const DRIVE_RAMDISK: u32 = 6;

/// 获取 diskpart 可执行文件路径
/// 优先使用内置的 diskpart，如果不存在则使用系统的
fn get_diskpart_path() -> String {
    let builtin_diskpart = get_bin_dir().join("diskpart").join("diskpart.exe");
    if builtin_diskpart.exists() {
        log::info!("使用内置 diskpart: {}", builtin_diskpart.display());
        builtin_diskpart.to_string_lossy().to_string()
    } else {
        log::debug!("使用系统 diskpart");
        "diskpart.exe".to_string()
    }
}

/// 自动创建分区的标志文件名
pub const AUTO_CREATED_PARTITION_MARKER: &str = "LetRecovery_AutoCreated.marker";

/// 分区表类型
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PartitionStyle {
    GPT,
    MBR,
    #[default]
    Unknown,
}

impl std::fmt::Display for PartitionStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionStyle::GPT => write!(f, "GPT"),
            PartitionStyle::MBR => write!(f, "MBR"),
            PartitionStyle::Unknown => write!(f, "未知"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub letter: String,
    pub total_size_mb: u64,
    pub free_size_mb: u64,
    pub label: String,
    pub is_system_partition: bool,
    pub has_windows: bool,
    pub partition_style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
    pub bitlocker_status: VolumeStatus,
}

/// 分区详细信息
#[derive(Debug, Clone)]
pub struct PartitionDetail {
    pub style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
}

/// STORAGE_DEVICE_NUMBER 结构
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

/// DRIVE_LAYOUT_INFORMATION_EX 结构（简化版，只需要头部信息）
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct DriveLayoutInformationEx {
    partition_style: u32,
    partition_count: u32,
    // union 部分我们不需要完整读取
}

pub struct DiskManager;

impl DiskManager {
    /// 获取所有固定磁盘分区列表
    pub fn get_partitions() -> Result<Vec<Partition>> {
        let mut partitions = Vec::new();
        let is_pe = Self::is_pe_environment();

        // 预先创建 BitLockerManager 实例，避免重复创建
        let bitlocker_manager = BitLockerManager::new();

        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            if let Ok(info) = Self::get_partition_info(&drive, is_pe, &bitlocker_manager) {
                partitions.push(info);
            }
        }

        Ok(partitions)
    }

    fn get_partition_info(drive: &str, is_pe: bool, bitlocker_manager: &BitLockerManager) -> Result<Partition> {
        let path = format!("{}\\", drive);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        #[cfg(windows)]
        {
            // 获取驱动器类型
            let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };
            if drive_type != DRIVE_FIXED {
                anyhow::bail!("Not a fixed drive");
            }
        }

        // 获取磁盘空间
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        #[cfg(windows)]
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available as *mut u64),
                Some(&mut total_bytes as *mut u64),
                Some(&mut total_free_bytes as *mut u64),
            )?;
        }

        // 获取卷标
        let mut volume_name = [0u16; 261];
        #[cfg(windows)]
        unsafe {
            let _ = GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut volume_name),
                None,
                None,
                None,
                None,
            );
        }
        let label = String::from_utf16_lossy(&volume_name)
            .trim_end_matches('\0')
            .to_string();

        // 检查是否为当前系统分区
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let is_current_system = drive.eq_ignore_ascii_case(&system_drive);

        // 检查是否包含 Windows 系统
        let windows_path = format!("{}\\Windows\\System32", drive);
        let has_windows = Path::new(&windows_path).exists();

        // 在 PE 环境下，is_system_partition 表示是否包含 Windows
        // 在正常环境下，is_system_partition 表示是否是当前系统盘
        let is_system_partition = if is_pe {
            has_windows && !is_current_system // PE下排除 X: 盘
        } else {
            is_current_system
        };

        // 获取分区表类型、磁盘号和分区号
        let detail = Self::get_partition_style(drive);

        // 获取 BitLocker 状态
        let letter_char = drive.chars().next().unwrap_or('C');
        let bitlocker_status = bitlocker_manager.get_status(letter_char);

        Ok(Partition {
            letter: drive.to_string(),
            total_size_mb: total_bytes / 1024 / 1024,
            free_size_mb: free_bytes_available / 1024 / 1024,
            label,
            is_system_partition,
            has_windows,
            partition_style: detail.style,
            disk_number: detail.disk_number,
            partition_number: detail.partition_number,
            bitlocker_status,
        })
    }

    /// 使用 Windows API 获取分区表类型和分区号 (GPT/MBR)
    #[cfg(windows)]
    fn get_partition_style(drive: &str) -> PartitionDetail {
        let letter = drive.chars().next().unwrap_or('C');
        
        // 先获取磁盘号和分区号
        let (disk_number, partition_number) = Self::get_device_number(letter);
        
        // 再获取分区表类型
        let style = if let Some(disk_num) = disk_number {
            Self::get_disk_partition_style_api(disk_num)
        } else {
            PartitionStyle::Unknown
        };

        PartitionDetail {
            style,
            disk_number,
            partition_number,
        }
    }

    #[cfg(not(windows))]
    fn get_partition_style(_drive: &str) -> PartitionDetail {
        PartitionDetail {
            style: PartitionStyle::Unknown,
            disk_number: None,
            partition_number: None,
        }
    }

    /// 使用 IOCTL_STORAGE_GET_DEVICE_NUMBER 获取磁盘号和分区号
    #[cfg(windows)]
    fn get_device_number(letter: char) -> (Option<u32>, Option<u32>) {
        unsafe {
            // 打开卷设备
            let volume_path = format!("\\\\.\\{}:", letter);
            let wide_path: Vec<u16> = volume_path.encode_utf16().chain(std::iter::once(0)).collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0, // 不需要读写权限，只需要查询
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return (None, None),
            };

            if handle == INVALID_HANDLE_VALUE {
                return (None, None);
            }

            let mut device_number = StorageDeviceNumber::default();
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                None,
                0,
                Some(&mut device_number as *mut _ as *mut _),
                std::mem::size_of::<StorageDeviceNumber>() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_ok() {
                (Some(device_number.device_number), Some(device_number.partition_number))
            } else {
                (None, None)
            }
        }
    }

    /// 使用 IOCTL_DISK_GET_DRIVE_LAYOUT_EX 获取磁盘分区表类型
    #[cfg(windows)]
    fn get_disk_partition_style_api(disk_number: u32) -> PartitionStyle {
        unsafe {
            // 打开物理磁盘
            let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_number);
            let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0, // 不需要读写权限
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return PartitionStyle::Unknown,
            };

            if handle == INVALID_HANDLE_VALUE {
                return PartitionStyle::Unknown;
            }

            // 分配足够大的缓冲区来存储分区布局信息
            // DRIVE_LAYOUT_INFORMATION_EX 的大小取决于分区数量
            // 我们只需要头部的 partition_style 字段
            let mut buffer = vec![0u8; 4096];
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                None,
                0,
                Some(buffer.as_mut_ptr() as *mut _),
                buffer.len() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_ok() && bytes_returned >= 8 {
                // 读取头部的 partition_style 字段（前4字节）
                let layout = &*(buffer.as_ptr() as *const DriveLayoutInformationEx);
                
                match layout.partition_style {
                    x if x == PARTITION_STYLE_MBR.0 as u32 => PartitionStyle::MBR,
                    x if x == PARTITION_STYLE_GPT.0 as u32 => PartitionStyle::GPT,
                    _ => PartitionStyle::Unknown,
                }
            } else {
                PartitionStyle::Unknown
            }
        }
    }

    /// 格式化指定分区
    pub fn format_partition(partition: &str) -> Result<String> {
        let bin_dir = get_bin_dir();
        let format_exe = if Self::is_pe_environment() {
            bin_dir.join("format.com").to_string_lossy().to_string()
        } else {
            "format.com".to_string()
        };

        let output = create_command(&format_exe)
            .args([partition, "/FS:NTFS", "/q", "/y"])
            .output()?;

        Ok(gbk_to_utf8(&output.stdout))
    }

    /// 从指定分区缩小并创建新分区
    pub fn shrink_and_create_partition(
        source_partition: &str,
        new_letter: &str,
        size_mb: u64,
    ) -> Result<String> {
        let script_content = format!(
            "select volume {}\nshrink desired={}\ncreate partition primary size={}\nformat fs=ntfs quick\nassign letter={}",
            source_partition.chars().next().unwrap_or('C'),
            size_mb,
            size_mb,
            new_letter.chars().next().unwrap_or('Y').to_ascii_lowercase()
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("dp_script.txt");
        std::fs::write(&script_path, &script_content)?;

        let output = create_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        Ok(gbk_to_utf8(&output.stdout))
    }

    /// 删除指定分区
    pub fn delete_partition(partition_letter: &str) -> Result<String> {
        let script_content = format!(
            "select volume {}\ndelete partition override",
            partition_letter.chars().next().unwrap_or('Y')
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("dp_delete.txt");
        std::fs::write(&script_path, &script_content)?;

        let output = create_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        Ok(gbk_to_utf8(&output.stdout))
    }

    /// 检查指定分区是否包含有效的 Windows 系统
    pub fn has_valid_windows(partition: &str) -> bool {
        let paths_to_check = [
            format!("{}\\Windows\\System32\\config\\SYSTEM", partition),
            format!("{}\\Windows\\System32\\config\\SOFTWARE", partition),
            format!("{}\\Windows\\explorer.exe", partition),
        ];

        paths_to_check.iter().all(|p| Path::new(p).exists())
    }

    /// 获取 Windows 版本信息（使用 Windows API）
    #[cfg(windows)]
    pub fn get_windows_version(partition: &str) -> Option<String> {
        use windows::Win32::Storage::FileSystem::GetFileVersionInfoSizeW;
        use windows::Win32::Storage::FileSystem::GetFileVersionInfoW;
        use windows::Win32::Storage::FileSystem::VerQueryValueW;

        let ntoskrnl = format!("{}\\Windows\\System32\\ntoskrnl.exe", partition);
        if !Path::new(&ntoskrnl).exists() {
            return None;
        }

        unsafe {
            let wide_path: Vec<u16> = ntoskrnl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut handle: u32 = 0;
            
            let size = GetFileVersionInfoSizeW(PCWSTR::from_raw(wide_path.as_ptr()), Some(&mut handle));
            if size == 0 {
                return None;
            }

            let mut buffer = vec![0u8; size as usize];
            let result = GetFileVersionInfoW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0,
                size,
                buffer.as_mut_ptr() as *mut _,
            );

            if result.is_err() {
                return None;
            }

            // 查询固定文件信息
            let sub_block: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
            let mut info_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut info_len: u32 = 0;

            let result = VerQueryValueW(
                buffer.as_ptr() as *const _,
                PCWSTR::from_raw(sub_block.as_ptr()),
                &mut info_ptr,
                &mut info_len,
            );

            if result.as_bool() && !info_ptr.is_null() {
                // VS_FIXEDFILEINFO 结构
                #[repr(C)]
                struct VsFixedFileInfo {
                    dw_signature: u32,
                    dw_struc_version: u32,
                    dw_file_version_ms: u32,
                    dw_file_version_ls: u32,
                    dw_product_version_ms: u32,
                    dw_product_version_ls: u32,
                    // ... 其他字段我们不需要
                }

                let info = &*(info_ptr as *const VsFixedFileInfo);
                let major = (info.dw_file_version_ms >> 16) & 0xFFFF;
                let minor = info.dw_file_version_ms & 0xFFFF;
                let build = (info.dw_file_version_ls >> 16) & 0xFFFF;
                let revision = info.dw_file_version_ls & 0xFFFF;

                return Some(format!("{}.{}.{}.{}", major, minor, build, revision));
            }

            None
        }
    }

    #[cfg(not(windows))]
    pub fn get_windows_version(_partition: &str) -> Option<String> {
        None
    }

    pub fn is_pe_environment() -> bool {
        crate::core::system_info::SystemInfo::check_pe_environment()
    }

    /// 检查指定盘符是否为光驱
    #[cfg(windows)]
    pub fn is_cdrom(letter: char) -> bool {
        let path = format!("{}:\\", letter);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let drive_type = GetDriveTypeW(PCWSTR(wide_path.as_ptr()));
            drive_type == DRIVE_CDROM
        }
    }

    #[cfg(not(windows))]
    pub fn is_cdrom(_letter: char) -> bool {
        false
    }

    /// 检查指定盘符是否为固定磁盘
    #[cfg(windows)]
    pub fn is_fixed_drive(letter: char) -> bool {
        let path = format!("{}:\\", letter);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let drive_type = GetDriveTypeW(PCWSTR(wide_path.as_ptr()));
            drive_type == DRIVE_FIXED
        }
    }

    #[cfg(not(windows))]
    pub fn is_fixed_drive(_letter: char) -> bool {
        false
    }

    /// 获取指定分区的剩余空间（字节）
    #[cfg(windows)]
    pub fn get_free_space_bytes(partition: &str) -> Option<u64> {
        let path = format!("{}\\", partition);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        unsafe {
            let result = GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available as *mut u64),
                Some(&mut total_bytes as *mut u64),
                Some(&mut total_free_bytes as *mut u64),
            );
            
            if result.is_ok() {
                Some(free_bytes_available)
            } else {
                None
            }
        }
    }

    #[cfg(not(windows))]
    pub fn get_free_space_bytes(_partition: &str) -> Option<u64> {
        None
    }

    /// 获取所有已使用的盘符
    pub fn get_used_drive_letters() -> Vec<char> {
        let mut letters = Vec::new();
        for letter in b'A'..=b'Z' {
            let c = letter as char;
            let path = format!("{}:\\", c);
            if Path::new(&path).exists() {
                letters.push(c);
            }
        }
        letters
    }

    /// 查找第一个可用的盘符（未被使用的）
    pub fn find_available_drive_letter() -> Option<char> {
        let used = Self::get_used_drive_letters();
        // 从后往前找，避开常用盘符
        for letter in ('E'..='Z').rev() {
            if !used.contains(&letter) {
                return Some(letter);
            }
        }
        // 如果都被占用，尝试 D
        if !used.contains(&'D') {
            return Some('D');
        }
        None
    }

    /// 查询指定分区可缩小的最大空间（MB）
    pub fn query_shrink_max(letter: char) -> Result<u64> {
        let script_content = format!(
            "select volume {}\nshrink querymax",
            letter
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_query_shrink.txt");
        std::fs::write(&script_path, &script_content)?;

        // 首先尝试使用内置 diskpart，如果失败则使用系统 diskpart
        let diskpart_path = get_diskpart_path();
        let output = create_command(&diskpart_path)
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let output_text = gbk_to_utf8(&output.stdout);
        let error_text = gbk_to_utf8(&output.stderr);
        
        println!("[DISK] Shrink querymax 使用: {}", diskpart_path);
        println!("[DISK] Shrink querymax stdout 长度: {} 字节", output.stdout.len());
        println!("[DISK] Shrink querymax 输出: {}", output_text);
        if !error_text.is_empty() {
            println!("[DISK] Shrink querymax 错误: {}", error_text);
        }

        // 如果输出为空且使用的是内置 diskpart，尝试使用系统 diskpart
        let output_text = if output_text.trim().is_empty() || output.stdout.len() < 50 {
            println!("[DISK] 内置 diskpart 输出异常，尝试使用系统 diskpart");
            
            let sys_output = create_command("diskpart.exe")
                .args(["/s", script_path.to_str().unwrap()])
                .output()?;
            
            let sys_output_text = gbk_to_utf8(&sys_output.stdout);
            println!("[DISK] 系统 diskpart stdout 长度: {} 字节", sys_output.stdout.len());
            println!("[DISK] 系统 diskpart 输出: {}", sys_output_text);
            
            sys_output_text
        } else {
            output_text
        };
        
        let _ = std::fs::remove_file(&script_path);

        // 解析输出，查找可回收的最大空间
        // 英文: "The maximum number of reclaimable bytes is: XXX MB"
        // 中文: "可回收的最大字节数为:  XXX MB" 或 "最多可从此卷收回 XXX MB"
        
        // 尝试多种模式匹配
        let max_mb = Self::parse_shrink_max_output(&output_text)
            .or_else(|| Self::parse_shrink_max_output_cn(&output_text))
            .or_else(|| Self::parse_shrink_max_generic(&output_text))
            .unwrap_or(0);

        println!("[DISK] 分区 {}: 可缩小的最大空间: {} MB", letter, max_mb);
        Ok(max_mb)
    }

    /// 解析 shrink querymax 输出（英文）
    fn parse_shrink_max_output(output: &str) -> Option<u64> {
        // 匹配 "XXX MB" 或 "XXX GB" 格式
        for line in output.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("reclaimable") || line_lower.contains("maximum") {
                // 提取数字
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if let Ok(num) = part.replace(",", "").parse::<u64>() {
                        // 检查单位
                        if i + 1 < parts.len() {
                            let unit = parts[i + 1].to_lowercase();
                            if unit.starts_with("gb") {
                                return Some(num * 1024);
                            } else if unit.starts_with("mb") {
                                return Some(num);
                            }
                        }
                        return Some(num); // 默认 MB
                    }
                }
            }
        }
        None
    }

    /// 解析 shrink querymax 输出（中文）
    fn parse_shrink_max_output_cn(output: &str) -> Option<u64> {
        for line in output.lines() {
            // 中文输出可能的格式：
            // "可回收的最大字节数为:  XXX MB"
            // "最多可从此卷收回 XXX MB"  
            // "可以从该卷收回的最大空间是: XXX MB"
            // "该卷可以收回的最大空间为 XXX MB"
            if line.contains("回收") || line.contains("收回") || line.contains("可用") 
                || line.contains("压缩") || line.contains("缩小") || line.contains("最大")
                || line.contains("空间") || line.contains("字节") {
                println!("[DISK] 尝试解析中文行: {}", line);
                if let Some(size) = Self::extract_size_from_line(line) {
                    println!("[DISK] 解析成功: {} MB", size);
                    return Some(size);
                }
            }
        }
        None
    }

    /// 通用解析：查找任何包含数字+MB/GB的行
    fn parse_shrink_max_generic(output: &str) -> Option<u64> {
        for line in output.lines() {
            // 跳过明显的非结果行
            let line_lower = line.to_lowercase();
            if line_lower.contains("diskpart") || line_lower.contains("microsoft") 
                || line_lower.contains("version") || line_lower.contains("volume")
                || line_lower.contains("select") || line.trim().is_empty() {
                continue;
            }
            
            if let Some(size) = Self::extract_size_from_line(line) {
                return Some(size);
            }
        }
        None
    }

    /// 从一行文本中提取大小（MB）
    fn extract_size_from_line(line: &str) -> Option<u64> {
        let mut num_str = String::new();
        let mut found_num = false;
        let chars: Vec<char> = line.chars().collect();
        
        for (i, c) in chars.iter().enumerate() {
            if c.is_ascii_digit() {
                num_str.push(*c);
                found_num = true;
            } else if found_num && *c == ',' {
                // 跳过千位分隔符
                continue;
            } else if found_num && !c.is_ascii_digit() {
                // 数字结束，检查单位
                if let Ok(num) = num_str.replace(",", "").parse::<u64>() {
                    if num == 0 {
                        num_str.clear();
                        found_num = false;
                        continue;
                    }
                    // 查找后面的单位
                    let rest: String = chars[i..].iter().collect();
                    let rest_lower = rest.to_lowercase();
                    if rest_lower.starts_with(" gb") || rest_lower.starts_with("gb") {
                        return Some(num * 1024);
                    } else if rest_lower.starts_with(" mb") || rest_lower.starts_with("mb") {
                        return Some(num);
                    } else if rest_lower.starts_with(" kb") || rest_lower.starts_with("kb") {
                        return Some(num / 1024);
                    }
                    // 如果数字较大（>100），假设是 MB
                    if num > 100 {
                        return Some(num);
                    }
                }
                num_str.clear();
                found_num = false;
            }
        }
        
        // 如果循环结束还有数字
        if !num_str.is_empty() {
            if let Ok(num) = num_str.parse::<u64>() {
                if num > 100 {
                    return Some(num);
                }
            }
        }
        
        None
    }

    /// 从指定分区缩小并创建新分区（增强版，带标志文件）
    /// 
    /// # Arguments
    /// * `source_letter` - 源分区盘符
    /// * `desired_size_mb` - 期望的新分区大小（MB）
    /// * `pre_queried_max_mb` - 预先查询的最大可缩小空间（MB），如果为 None 则内部查询
    /// 
    /// # Returns
    /// * `Ok(char)` - 新分区的盘符
    /// * `Err` - 错误信息
    pub fn shrink_and_create_partition_with_marker(
        source_letter: char,
        desired_size_mb: u64,
        pre_queried_max_mb: Option<u64>,
    ) -> Result<char> {
        // 使用预查询的值或者重新查询
        let max_shrink_mb = match pre_queried_max_mb {
            Some(mb) => mb,
            None => Self::query_shrink_max(source_letter)?,
        };
        
        if max_shrink_mb == 0 {
            anyhow::bail!(
                "分区 {}: 无法缩小，可能需要先进行碎片整理。\n\
                建议：在 Windows 中运行磁盘碎片整理工具，或使用其他分区工具。",
                source_letter
            );
        }

        // 使用实际可缩小的空间
        let actual_size_mb = if desired_size_mb > max_shrink_mb {
            println!(
                "[DISK] 警告: 期望缩小 {} MB，但最多只能缩小 {} MB，将使用最大可用值",
                desired_size_mb, max_shrink_mb
            );
            max_shrink_mb
        } else {
            desired_size_mb
        };

        // 确保至少有 1GB 可用
        if actual_size_mb < 1024 {
            anyhow::bail!(
                "分区 {}: 可缩小空间太小（{} MB），需要至少 1024 MB (1 GB)。\n\
                建议：清理磁盘空间或进行碎片整理后重试。",
                source_letter, actual_size_mb
            );
        }

        // 找一个可用的盘符
        let new_letter = Self::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("没有可用的盘符"))?;

        println!(
            "[DISK] 准备从 {}: 缩小 {} MB 并创建新分区 {}:",
            source_letter, actual_size_mb, new_letter
        );

        // 使用 diskpart 执行操作
        // 注意：shrink 之后的未分配空间会紧跟在当前卷之后
        let script_content = format!(
            "select volume {}\n\
            shrink desired={}\n\
            create partition primary\n\
            format fs=ntfs quick label=\"LetRecovery\"\n\
            assign letter={}",
            source_letter,
            actual_size_mb,
            new_letter
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_shrink_script.txt");
        std::fs::write(&script_path, &script_content)?;

        println!("[DISK] Diskpart 脚本内容:\n{}", script_content);

        let output = create_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        let output_text = gbk_to_utf8(&output.stdout);
        let error_text = gbk_to_utf8(&output.stderr);

        println!("[DISK] Diskpart 输出: {}", output_text);
        if !error_text.is_empty() {
            println!("[DISK] Diskpart 错误: {}", error_text);
        }

        // 检查输出是否包含错误信息
        let output_lower = output_text.to_lowercase();
        if output_lower.contains("error") || output_lower.contains("错误") 
            || output_lower.contains("失败") || output_lower.contains("failed")
            || output_lower.contains("无效") || output_lower.contains("invalid")
            || output_lower.contains("不支持") || output_lower.contains("无法")
            || output_lower.contains("拒绝") || output_lower.contains("denied") {
            anyhow::bail!("Diskpart 执行失败: {}", output_text);
        }

        // 等待系统识别新分区
        std::thread::sleep(std::time::Duration::from_secs(2));

        // 验证新分区是否创建成功
        let new_partition_path = format!("{}:\\", new_letter);
        for retry in 0..5 {
            if Path::new(&new_partition_path).exists() {
                break;
            }
            if retry == 4 {
                anyhow::bail!(
                    "分区创建失败：新分区 {}: 不可访问。\n\
                    Diskpart 输出: {}",
                    new_letter, output_text
                );
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        // 写入标志文件
        let marker_path = format!("{}:\\{}", new_letter, AUTO_CREATED_PARTITION_MARKER);
        std::fs::write(
            &marker_path,
            format!(
                "LetRecovery Auto Created Partition\n\
                Created: {}\n\
                Source: {}:\n\
                Size: {} MB\n\
                Note: This partition was automatically created and can be safely deleted after system installation.",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                source_letter,
                actual_size_mb
            ),
        )
        .map_err(|e| anyhow::anyhow!("写入标志文件失败: {}", e))?;

        println!(
            "[DISK] 新分区 {}: 创建成功，大小 {} MB，标志文件已写入",
            new_letter, actual_size_mb
        );

        Ok(new_letter)
    }

    /// 检查分区是否是自动创建的（通过检查标志文件）
    pub fn is_auto_created_partition(letter: char) -> bool {
        let marker_path = format!("{}:\\{}", letter, AUTO_CREATED_PARTITION_MARKER);
        Path::new(&marker_path).exists()
    }

    /// 删除自动创建的分区
    pub fn delete_auto_created_partition(letter: char) -> Result<()> {
        if !Self::is_auto_created_partition(letter) {
            anyhow::bail!("分区 {} 不是自动创建的分区", letter);
        }

        println!("[DISK] 准备删除自动创建的分区 {}:", letter);

        let script_content = format!(
            "select volume {}\ndelete partition override",
            letter
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_delete_script.txt");
        std::fs::write(&script_path, &script_content)?;

        let output = create_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        let output_text = gbk_to_utf8(&output.stdout);
        println!("[DISK] Diskpart 删除输出: {}", output_text);

        Ok(())
    }

    /// 查找可用的数据分区（排除指定分区、光驱，检查空间）
    /// 
    /// # Arguments
    /// * `exclude_partition` - 要排除的分区（通常是目标安装分区）
    /// * `required_size_bytes` - 需要的最小空间（字节）
    /// 
    /// # Returns
    /// * `Ok(Some((partition, is_auto_created)))` - 找到可用分区，返回分区盘符和是否是自动创建的
    /// * `Ok(None)` - 没有找到可用分区，且无法自动创建
    /// * `Err` - 发生错误
    pub fn find_suitable_data_partition(
        exclude_partition: &str,
        required_size_bytes: u64,
    ) -> Result<Option<(String, bool)>> {
        let exclude_letter = exclude_partition.chars().next().unwrap_or('C').to_ascii_uppercase();
        
        println!("[DISK] 查找数据分区，排除: {}, 需要空间: {} bytes ({:.2} GB)", 
            exclude_partition, 
            required_size_bytes,
            required_size_bytes as f64 / 1024.0 / 1024.0 / 1024.0
        );

        // 第一遍：查找所有可用的固定磁盘分区（排除光驱、排除目标分区）
        let mut candidates: Vec<(char, u64)> = Vec::new();
        
        for letter in b'A'..=b'Z' {
            let c = letter as char;
            
            // 跳过排除的分区
            if c == exclude_letter {
                continue;
            }
            
            // 跳过 X 盘（PE 系统盘）
            if c == 'X' {
                continue;
            }

            let partition_path = format!("{}:\\", c);
            if !Path::new(&partition_path).exists() {
                continue;
            }

            // 检查是否为光驱
            if Self::is_cdrom(c) {
                println!("[DISK] 跳过光驱: {}:", c);
                continue;
            }

            // 检查是否为固定磁盘
            if !Self::is_fixed_drive(c) {
                println!("[DISK] 跳过非固定磁盘: {}:", c);
                continue;
            }

            // 获取剩余空间
            if let Some(free_space) = Self::get_free_space_bytes(&format!("{}:", c)) {
                println!("[DISK] 分区 {}:  剩余空间: {} bytes ({:.2} GB)", 
                    c, free_space, free_space as f64 / 1024.0 / 1024.0 / 1024.0);
                
                if free_space >= required_size_bytes {
                    candidates.push((c, free_space));
                }
            }
        }

        // 如果找到了满足条件的分区，优先选择非 C 盘，且空间最大的
        if !candidates.is_empty() {
            // 按优先级排序：非 C 盘优先，然后按空间从大到小
            candidates.sort_by(|a, b| {
                let a_is_c = a.0 == 'C';
                let b_is_c = b.0 == 'C';
                match (a_is_c, b_is_c) {
                    (true, false) => std::cmp::Ordering::Greater,  // C 盘排后面
                    (false, true) => std::cmp::Ordering::Less,    // 非 C 盘排前面
                    _ => b.1.cmp(&a.1),  // 空间大的优先
                }
            });

            let selected = candidates[0].0;
            println!("[DISK] 选择数据分区: {}:", selected);
            return Ok(Some((format!("{}:", selected), false)));
        }

        // ========================================================================
        // 没有找到满足条件的现有分区，尝试从目标安装分区创建新分区
        // ========================================================================
        // 
        // ⚠️ 重要：这里【不能】检查 exclude_letter == 'C' 然后直接返回！
        // 
        // 错误的写法（已删除）：
        //   if exclude_letter == 'C' {
        //       return Ok(None);  // ← 这是错的！
        //   }
        //
        // 原因：当用户只有一个 C 盘时（比如虚拟机环境），需要从 C 盘分割出
        // 一个临时分区来存放镜像文件。PE 安装流程如下：
        //   1. 从 C 盘分割出临时分区（如 Y:）
        //   2. 将镜像复制到 Y:\LetRecovery\
        //   3. 重启进入 PE
        //   4. PE 中格式化 C 盘并释放镜像
        //   5. 安装完成后可删除 Y: 分区
        //
        // 因此，即使 exclude_letter == 'C'，也必须尝试分割 C 盘！
        // ========================================================================
        println!("[DISK] 没有找到满足条件的现有分区，尝试从 {} 盘创建新分区", exclude_letter);

        // 使用 shrink querymax 查询目标分区实际可缩小的空间
        let max_shrink_mb = match Self::query_shrink_max(exclude_letter) {
            Ok(mb) => mb,
            Err(e) => {
                println!("[DISK] 查询 {} 盘可缩小空间失败: {}", exclude_letter, e);
                return Ok(None);
            }
        };

        let max_shrink_bytes = max_shrink_mb * 1024 * 1024;
        println!("[DISK] {} 盘实际可缩小空间: {} MB ({:.2} GB)", 
            exclude_letter, max_shrink_mb, max_shrink_bytes as f64 / 1024.0 / 1024.0 / 1024.0);

        // 检查可缩小空间是否足够容纳镜像
        if max_shrink_bytes < required_size_bytes {
            println!("[DISK] {} 盘可缩小空间不足以容纳镜像文件", exclude_letter);
            return Err(anyhow::anyhow!(
                "磁盘空间不足：{} 盘可缩小空间为 {:.2} GB，但镜像需要 {:.2} GB。\n\
                建议：\n\
                1. 清理 {} 盘空间\n\
                2. 运行磁盘碎片整理\n\
                3. 或手动创建一个数据分区",
                exclude_letter,
                max_shrink_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                required_size_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                exclude_letter
            ));
        }

        // 计算新分区大小
        // 理想大小 = 镜像大小 + 10GB，向上取整到整数 GB
        let required_size_mb = (required_size_bytes + 1024 * 1024 - 1) / (1024 * 1024); // 向上取整到 MB
        let ten_gb_mb: u64 = 10 * 1024; // 10GB in MB
        let ideal_size_mb = required_size_mb + ten_gb_mb;
        
        // 向上取整到整数 GB
        let ideal_size_gb = (ideal_size_mb + 1023) / 1024;
        let ideal_size_mb_rounded = ideal_size_gb * 1024;

        let actual_size_mb: u64;
        
        if max_shrink_mb >= ideal_size_mb_rounded {
            // 可缩小空间充足，使用理想大小（镜像 + 10GB 缓冲）
            actual_size_mb = ideal_size_mb_rounded;
            println!("[DISK] 使用理想分区大小: {} MB ({} GB)", actual_size_mb, ideal_size_gb);
        } else {
            // 可缩小空间不足以达到理想大小
            // 确保至少能容纳镜像文件，向上取整到整数 GB
            let min_size_gb = (required_size_mb + 1023) / 1024; // 向上取整
            let available_size_gb = max_shrink_mb / 1024; // 可用的整数 GB
            
            if available_size_gb >= min_size_gb {
                // 使用可用的整数 GB
                actual_size_mb = available_size_gb * 1024;
                println!("[DISK] 可缩小空间有限，使用较小分区大小: {} MB ({} GB)", actual_size_mb, available_size_gb);
            } else {
                // 整数 GB 不够，直接使用全部可缩小空间（不取整）
                actual_size_mb = max_shrink_mb;
                println!("[DISK] 空间紧张，使用全部可缩小空间: {} MB ({:.2} GB)", actual_size_mb, max_shrink_mb as f64 / 1024.0);
            }
        }

        // 确保分区大小至少为 1GB 且能容纳镜像
        if actual_size_mb < 1024 {
            return Err(anyhow::anyhow!(
                "{} 盘可缩小空间太小（{} MB），需要至少 1 GB。\n\
                建议运行磁盘碎片整理后重试。",
                exclude_letter,
                max_shrink_mb
            ));
        }

        if actual_size_mb * 1024 * 1024 < required_size_bytes {
            return Err(anyhow::anyhow!(
                "{} 盘可缩小空间（{} MB）不足以容纳镜像（需要 {} MB）。",
                exclude_letter,
                actual_size_mb,
                required_size_mb
            ));
        }

        // 创建新分区（传入预查询的 max_shrink_mb，避免重复查询）
        let new_letter = Self::shrink_and_create_partition_with_marker(exclude_letter, actual_size_mb, Some(max_shrink_mb))?;
        
        Ok(Some((format!("{}:", new_letter), true)))
    }
}
