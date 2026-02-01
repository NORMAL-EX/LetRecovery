use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW};

use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

const DRIVE_FIXED: u32 = 3;

/// 自动创建分区的标志文件名
pub const AUTO_CREATED_PARTITION_MARKER: &str = "LetRecovery_AutoCreated.marker";

/// 获取 diskpart 可执行文件路径
/// 优先使用内置的 diskpart，如果不存在则使用系统的
fn get_diskpart_path() -> String {
    let builtin_diskpart = get_bin_dir().join("diskpart").join("diskpart.exe");
    if builtin_diskpart.exists() {
        log::info!("使用内置 diskpart: {}", builtin_diskpart.display());
        builtin_diskpart.to_string_lossy().to_string()
    } else {
        log::info!("使用系统 diskpart");
        "diskpart.exe".to_string()
    }
}

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
}

/// 分区详细信息
#[derive(Debug, Clone)]
pub struct PartitionDetail {
    pub style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
}

pub struct DiskManager;

impl DiskManager {

    /// 选择一个可靠的临时目录并确保它存在。
    /// WinPE 下 std::env::temp_dir() 可能指向不存在的路径，
    /// 直接写 diskpart 脚本会触发 "系统找不到指定的路径 (os error 3)"。
    fn reliable_temp_dir() -> PathBuf {
        // WinPE 下最稳的两个临时目录：X:\Windows\Temp / X:\Temp
        // 注意：WinPE 里 std::env::temp_dir() 可能指向不存在的路径，所以这里要“尽力创建”。
        let candidates = [
            PathBuf::from(r"X:\Windows\Temp"),
            PathBuf::from(r"X:\Temp"),
            std::env::temp_dir(),
            PathBuf::from("X:\\"),
        ];

        for dir in candidates {
            let _ = fs::create_dir_all(&dir);
            if dir.exists() {
                return dir;
            }
        }

        // 兜底：即便不存在也返回一个值，避免上层因为 ? 直接编译不过/崩溃。
        std::env::temp_dir()
    }
    /// 获取所有固定磁盘分区列表
    pub fn get_partitions() -> Result<Vec<Partition>> {
        let mut partitions = Vec::new();

        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            if let Ok(info) = Self::get_partition_info(&drive) {
                log::debug!(
                    "Partition {} label=\"{}\" total={}MB free={}MB system={} windows={} style={}",
                    info.letter.as_str(),
                    info.label.as_str(),
                    info.total_size_mb,
                    info.free_size_mb,
                    info.is_system_partition,
                    info.has_windows,
                    info.partition_style
                );
                partitions.push(info);
            }
        }

        Ok(partitions)
    }

    fn get_partition_info(drive: &str) -> Result<Partition> {
        let path = format!("{}\\", drive);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        // 获取驱动器类型
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };
        if drive_type != DRIVE_FIXED {
            anyhow::bail!("Not a fixed drive");
        }

        // 获取磁盘空间
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

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

        // PE环境下排除 X: 盘
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "X:".to_string());
        let is_current_system = drive.eq_ignore_ascii_case(&system_drive);

        // 检查是否包含 Windows 系统
        let windows_path = format!("{}\\Windows\\System32", drive);
        let has_windows = Path::new(&windows_path).exists();

        // PE环境下，is_system_partition 表示是否包含 Windows（排除PE自己的X盘）
        let is_system_partition = has_windows && !is_current_system;

        // 获取分区表类型、磁盘号和分区号
        let detail = Self::get_partition_style(drive);

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
        })
    }

    /// 获取分区表类型和分区号 (GPT/MBR)
    fn get_partition_style(drive: &str) -> PartitionDetail {
        // PE环境下直接使用 diskpart
        Self::get_partition_style_diskpart(drive)
    }

    /// 使用 diskpart 获取分区信息（备用方法）
    fn get_partition_style_diskpart(drive: &str) -> PartitionDetail {
        let letter = drive.chars().next().unwrap_or('C');
        let script = format!("select volume {}\ndetail volume", letter);

        let temp_dir = Self::reliable_temp_dir();
        let script_path = temp_dir.join("dp_style.txt");

        if std::fs::write(&script_path, &script).is_err() {
            return PartitionDetail {
                style: PartitionStyle::Unknown,
                disk_number: None,
                partition_number: None,
            };
        }

        let output = match new_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()
        {
            Ok(o) => o,
            Err(_) => {
                let _ = std::fs::remove_file(&script_path);
                return PartitionDetail {
                    style: PartitionStyle::Unknown,
                    disk_number: None,
                    partition_number: None,
                };
            }
        };

        let _ = std::fs::remove_file(&script_path);
        let stdout = gbk_to_utf8(&output.stdout);

        let mut disk_num: Option<u32> = None;
        let mut part_num: Option<u32> = None;

        for line in stdout.lines() {
            let line_upper = line.to_uppercase();
            if (line_upper.contains("磁盘") || line_upper.contains("DISK"))
                && !line_upper.contains("磁盘 ID")
                && !line_upper.contains("DISK ID")
            {
                if let Some(num) = line
                    .split_whitespace()
                    .find(|s| s.parse::<u32>().is_ok())
                {
                    disk_num = num.parse().ok();
                }
            }
            if line_upper.contains("分区") || line_upper.contains("PARTITION") {
                if let Some(num) = line
                    .split_whitespace()
                    .find(|s| s.parse::<u32>().is_ok())
                {
                    part_num = num.parse().ok();
                }
            }
        }

        let style = if let Some(num) = disk_num {
            Self::get_disk_partition_style(num)
        } else {
            PartitionStyle::Unknown
        };

        PartitionDetail {
            style,
            disk_number: disk_num,
            partition_number: part_num,
        }
    }

    /// 获取指定磁盘的分区表类型
    fn get_disk_partition_style(disk_number: u32) -> PartitionStyle {
        let script = format!("select disk {}\ndetail disk", disk_number);
        let temp_dir = Self::reliable_temp_dir();
        let script_path = temp_dir.join("dp_disk_style.txt");

        if std::fs::write(&script_path, &script).is_err() {
            return PartitionStyle::Unknown;
        }

        let output = match new_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()
        {
            Ok(o) => o,
            Err(_) => {
                let _ = std::fs::remove_file(&script_path);
                return PartitionStyle::Unknown;
            }
        };

        let _ = std::fs::remove_file(&script_path);
        let stdout = gbk_to_utf8(&output.stdout).to_uppercase();

        if stdout.contains("GPT") {
            PartitionStyle::GPT
        } else if stdout.contains("MBR") {
            PartitionStyle::MBR
        } else {
            PartitionStyle::Unknown
        }
    }

    /// 格式化指定分区
    pub fn format_partition(partition: &str) -> Result<String> {
        Self::format_partition_with_label(partition, None)
    }
    
    /// 格式化指定分区（带卷标）
    /// 
    /// 使用 cmd /c format 进行格式化，因为直接调用 format.com 在 CREATE_NO_WINDOW 模式下
    /// 会完成格式化但进程不退出，导致程序卡死。通过 cmd /c 包装可以正常退出。
    pub fn format_partition_with_label(partition: &str, volume_label: Option<&str>) -> Result<String> {
        log::info!("格式化分区: {} 卷标: {:?}", partition, volume_label);

        // 提取盘符
        let drive_letter = partition
            .chars()
            .next()
            .unwrap_or('C')
            .to_ascii_uppercase();

        let drive = format!("{}:", drive_letter);

        // 卷标处理
        let vol_label = match volume_label {
            Some(label) if !label.is_empty() => label,
            _ => "本地磁盘",
        };

        // 使用 cmd /c format 命令: format D: /FS:NTFS /V:Label /Q /Y
        let cmd_args = format!("format {} /FS:NTFS /V:{} /Q /Y", drive, vol_label);
        
        log::info!("执行命令: cmd /c {}", cmd_args);

        let output = new_command("cmd")
            .args(["/c", &cmd_args])
            .output()?;

        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);

        log::info!("format 输出:\n{}", stdout);
        if !stderr.is_empty() {
            log::warn!("format 错误输出:\n{}", stderr);
        }

        // 检查执行结果
        let stdout_lower = stdout.to_lowercase();
        let success_indicators = ["格式化完成", "format complete", "已完成", "complete"];
        let has_success_indicator = success_indicators
            .iter()
            .any(|s| stdout_lower.contains(&s.to_lowercase()));
        
        if output.status.success() || has_success_indicator {
            log::info!("分区 {} 格式化成功", drive);
            Ok(stdout)
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
            anyhow::bail!("{}", error_msg);
        }
    }

    /// 检测是否为UEFI模式
    pub fn detect_uefi_mode() -> bool {
        // 检查EFI系统分区
        for letter in ['S', 'T', 'U', 'V', 'W', 'Y', 'Z'] {
            let efi_path = format!("{}:\\EFI\\Microsoft\\Boot", letter);
            if Path::new(&efi_path).exists() {
                return true;
            }
        }

        // 检查固件类型
        let output = new_command("cmd")
            .args(["/c", "bcdedit /enum firmware"])
            .output();

        if let Ok(output) = output {
            let stdout = gbk_to_utf8(&output.stdout);
            if stdout.contains("firmware") || stdout.contains("UEFI") {
                return true;
            }
        }

        false
    }

    /// 查找自动创建的分区（通过标志文件）
    /// 返回 (盘符, 磁盘号Option) 如果找到的话
    pub fn find_auto_created_partition() -> Option<(char, Option<u32>)> {
        for letter in b'A'..=b'Z' {
            let c = letter as char;
            // 跳过 X 盘（PE系统盘）
            if c == 'X' {
                continue;
            }
            
            let marker_path = format!("{}:\\{}", c, AUTO_CREATED_PARTITION_MARKER);
            if Path::new(&marker_path).exists() {
                log::info!("找到自动创建的分区: {}:", c);
                
                // 获取该分区所在的磁盘号
                let detail = Self::get_partition_style(&format!("{}:", c));
                return Some((c, detail.disk_number));
            }
        }
        None
    }

    /// 删除自动创建的分区并扩展目标分区
    /// 
    /// # Arguments
    /// * `target_partition` - 目标安装分区（如 "D:"），删除数据分区后要扩展的分区
    /// 
    /// 流程：
    /// 1. 找到自动创建的分区
    /// 2. 确认该分区和目标分区在同一个磁盘上
    /// 3. 检查分区号，确保临时分区在目标分区之后（相邻性检查）
    /// 4. 记录目标分区当前大小
    /// 5. 删除该分区
    /// 6. 刷新磁盘信息
    /// 7. 扩展目标分区以使用释放的空间
    /// 8. 验证分区大小是否增加
    pub fn cleanup_auto_created_partition_and_extend(target_partition: &str) -> Result<()> {
        let target_letter = target_partition.chars().next().unwrap_or('C').to_ascii_uppercase();
        
        log::info!("[CLEANUP] ========================================");
        log::info!("[CLEANUP] 开始清理自动创建的分区");
        log::info!("[CLEANUP] 目标安装分区: {}:", target_letter);
        log::info!("[CLEANUP] ========================================");

        // 查找自动创建的分区
        let (auto_letter, auto_disk_num_opt) = match Self::find_auto_created_partition() {
            Some(info) => info,
            None => {
                log::info!("[CLEANUP] 未找到自动创建的分区，无需清理");
                return Ok(());
            }
        };

        // 获取自动创建分区的详细信息
        let auto_detail = Self::get_partition_style(&format!("{}:", auto_letter));
        let auto_disk_num = match auto_disk_num_opt.or(auto_detail.disk_number) {
            Some(num) => num,
            None => {
                log::warn!("[CLEANUP] 无法获取自动创建分区 {} 的磁盘号，只删除不扩展", auto_letter);
                return Self::delete_partition_by_letter(auto_letter);
            }
        };
        let auto_part_num = auto_detail.partition_number;

        log::info!(
            "[CLEANUP] 找到自动创建的分区: {}:, 磁盘 {}, 分区号 {:?}",
            auto_letter, auto_disk_num, auto_part_num
        );

        // 获取目标分区所在的磁盘号和分区号
        let target_detail = Self::get_partition_style(&format!("{}:", target_letter));
        let target_disk_num = match target_detail.disk_number {
            Some(num) => num,
            None => {
                log::warn!("[CLEANUP] 无法获取目标分区 {} 的磁盘号，只删除分区不扩展", target_letter);
                return Self::delete_partition_by_letter(auto_letter);
            }
        };
        let target_part_num = target_detail.partition_number;

        log::info!(
            "[CLEANUP] 目标分区: {}:, 磁盘 {}, 分区号 {:?}",
            target_letter, target_disk_num, target_part_num
        );

        // 检查是否在同一磁盘
        if auto_disk_num != target_disk_num {
            log::warn!(
                "[CLEANUP] 自动创建的分区 (磁盘{}) 和目标分区 (磁盘{}) 不在同一磁盘，只删除分区不扩展",
                auto_disk_num, target_disk_num
            );
            return Self::delete_partition_by_letter(auto_letter);
        }

        // 检查分区相邻性：临时分区应该在目标分区之后
        // diskpart extend 只能向后扩展到相邻的未分配空间
        if let (Some(target_pn), Some(auto_pn)) = (target_part_num, auto_part_num) {
            if auto_pn <= target_pn {
                log::warn!(
                    "[CLEANUP] 临时分区 (分区号{}) 在目标分区 (分区号{}) 之前或相同位置",
                    auto_pn, target_pn
                );
                log::warn!("[CLEANUP] extend 命令只能向后扩展，删除后的空间可能无法自动合并");
                log::warn!("[CLEANUP] 将只删除分区，用户可在安装完成后使用磁盘管理工具手动合并");
                return Self::delete_partition_by_letter(auto_letter);
            }
            
            // 检查是否相邻（分区号相差1）
            if auto_pn != target_pn + 1 {
                log::warn!(
                    "[CLEANUP] 临时分区 (分区号{}) 与目标分区 (分区号{}) 不相邻",
                    auto_pn, target_pn
                );
                log::warn!("[CLEANUP] 它们之间可能有其他分区，extend 可能无法成功");
            } else {
                log::info!("[CLEANUP] 分区相邻性检查通过：目标分区{} -> 临时分区{}", target_pn, auto_pn);
            }
        }

        // 删除自动创建分区并扩展目标分区
        log::info!("[CLEANUP] 开始删除分区 {} 并扩展目标分区 {}...", auto_letter, target_letter);
        Self::delete_partition_and_extend(auto_letter, target_letter, auto_disk_num)
    }

    /// 删除指定盘符的分区
    fn delete_partition_by_letter(letter: char) -> Result<()> {
        log::info!("[CLEANUP] 删除分区 {}:", letter);

        let script_content = format!(
            "select volume {}\ndelete partition override",
            letter
        );

        let temp_dir = Self::reliable_temp_dir();
        let script_path = temp_dir.join("lr_delete_part.txt");
        std::fs::write(&script_path, &script_content)?;

        let output = new_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        let output_text = gbk_to_utf8(&output.stdout);
        log::info!("[CLEANUP] Diskpart 删除输出: {}", output_text);

        // 检查是否有错误（但不要太严格，删除成功也可能包含一些警告）
        let output_lower = output_text.to_lowercase();
        let has_error = (output_lower.contains("error") || output_lower.contains("错误"))
            && !output_lower.contains("成功") && !output_lower.contains("successfully");
        
        if has_error {
            anyhow::bail!("删除分区失败: {}", output_text);
        }

        log::info!("[CLEANUP] 分区 {} 删除成功", letter);
        Ok(())
    }

    /// 获取分区大小（MB）
    fn get_partition_size_mb(letter: char) -> Option<u64> {
        let path = format!("{}:\\", letter);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        
        let mut total_bytes: u64 = 0;
        
        unsafe {
            let result = GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                None,
                Some(&mut total_bytes as *mut u64),
                None,
            );
            
            if result.is_ok() {
                Some(total_bytes / 1024 / 1024)
            } else {
                None
            }
        }
    }

    /// 删除分区并扩展目标分区
    fn delete_partition_and_extend(auto_letter: char, target_letter: char, disk_num: u32) -> Result<()> {
        // 记录扩展前的分区大小
        let size_before = Self::get_partition_size_mb(target_letter);
        log::info!("[CLEANUP] 扩展前目标分区大小: {:?} MB", size_before);

        // Step 1: 删除分区
        log::info!("[CLEANUP] Step 1: 删除分区 {}:", auto_letter);
        
        let delete_script = format!(
            "select volume {}\ndelete partition override",
            auto_letter
        );

        let temp_dir = Self::reliable_temp_dir();
        let script_path = temp_dir.join("lr_delete_part.txt");
        std::fs::write(&script_path, &delete_script)?;

        let output = new_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        let output_text = gbk_to_utf8(&output.stdout);
        log::info!("[CLEANUP] 删除分区输出: {}", output_text);

        // 检查删除是否成功
        let output_lower = output_text.to_lowercase();
        let delete_failed = (output_lower.contains("error") || output_lower.contains("错误")
            || output_lower.contains("失败") || output_lower.contains("failed"))
            && !output_lower.contains("成功") && !output_lower.contains("successfully");
            
        if delete_failed {
            anyhow::bail!("删除分区失败: {}", output_text);
        }

        log::info!("[CLEANUP] 分区 {} 删除成功", auto_letter);

        // Step 2: 运行 rescan 命令刷新磁盘信息
        log::info!("[CLEANUP] Step 2: 刷新磁盘信息 (rescan)");
        Self::diskpart_rescan();
        
        // 等待系统处理 rescan
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Step 3: 等待系统识别未分配空间，然后扩展目标分区（带重试）
        log::info!("[CLEANUP] Step 3: 扩展目标分区 {}（带重试）", target_letter);
        
        const MAX_RETRIES: u32 = 10;  // 增加到 10 次
        const RETRY_DELAY_SECS: u64 = 3;  // 增加到 3 秒
        
        let mut last_error = String::new();
        
        for attempt in 1..=MAX_RETRIES {
            log::info!("[CLEANUP] 扩展分区 {} 尝试 {}/{}", target_letter, attempt, MAX_RETRIES);
            
            // 尝试扩展
            match Self::try_extend_volume_enhanced(target_letter, disk_num) {
                Ok(_) => {
                    // 验证扩展是否成功
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let size_after = Self::get_partition_size_mb(target_letter);
                    log::info!("[CLEANUP] 扩展后目标分区大小: {:?} MB", size_after);
                    
                    if let (Some(before), Some(after)) = (size_before, size_after) {
                        if after > before {
                            log::info!("[CLEANUP] 分区 {} 扩展成功！大小从 {} MB 增加到 {} MB", 
                                target_letter, before, after);
                            return Ok(());
                        } else {
                            // extend 命令返回成功但分区大小未变化
                            // 可能是系统还未识别到未分配空间，继续重试
                            last_error = format!("extend 命令执行成功但分区大小未变化 (before={} MB, after={} MB)", before, after);
                            log::warn!("[CLEANUP] {}", last_error);
                        }
                    } else {
                        // 无法获取大小进行比较，假设成功
                        log::info!("[CLEANUP] 分区 {} 扩展命令执行成功（无法验证大小变化）", target_letter);
                        return Ok(());
                    }
                }
                Err(e) => {
                    last_error = e.to_string();
                    log::warn!("[CLEANUP] 扩展尝试 {} 失败: {}", attempt, e);
                }
            }
            
            if attempt < MAX_RETRIES {
                log::info!("[CLEANUP] 等待 {} 秒后重试...", RETRY_DELAY_SECS);
                std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));
                
                // 每 3 次尝试后再 rescan 一次
                if attempt % 3 == 0 {
                    log::info!("[CLEANUP] 再次刷新磁盘信息...");
                    Self::diskpart_rescan();
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        }

        // 所有重试都失败了
        log::warn!("[CLEANUP] ========================================");
        log::warn!("[CLEANUP] 分区扩展失败！");
        log::warn!("[CLEANUP] 目标分区: {}:", target_letter);
        log::warn!("[CLEANUP] 最后错误: {}", last_error);
        log::warn!("[CLEANUP] 数据分区已删除，但空间未能自动合并。");
        log::warn!("[CLEANUP] 用户可在系统安装完成后使用磁盘管理工具手动扩展分区。");
        log::warn!("[CLEANUP] ========================================");
        Ok(())
    }

    /// 运行 diskpart rescan 命令刷新磁盘信息
    fn diskpart_rescan() {
        let script_content = "rescan";
        let temp_dir = Self::reliable_temp_dir();
        let script_path = temp_dir.join("lr_rescan.txt");
        
        if std::fs::write(&script_path, script_content).is_ok() {
            let output = new_command(&get_diskpart_path())
                .args(["/s", script_path.to_str().unwrap()])
                .output();
            
            let _ = std::fs::remove_file(&script_path);
            
            if let Ok(output) = output {
                let output_text = gbk_to_utf8(&output.stdout);
                log::info!("[CLEANUP] rescan 输出: {}", output_text);
            }
        }
    }

    /// 尝试扩展指定分区（增强版，使用 diskpart）
    /// 先尝试通过卷字母扩展，如果失败则尝试通过磁盘号和分区号扩展
    fn try_extend_volume_enhanced(letter: char, disk_num: u32) -> Result<()> {
        // 方法1：通过卷字母扩展（标准方法）
        let extend_script = format!("select volume {}\nextend", letter);
        
        let temp_dir = Self::reliable_temp_dir();
        let script_path = temp_dir.join("lr_extend.txt");
        std::fs::write(&script_path, &extend_script)?;

        let output = new_command(&get_diskpart_path())
            .args(["/s", script_path.to_str().unwrap()])
            .output()?;

        let _ = std::fs::remove_file(&script_path);

        let output_text = gbk_to_utf8(&output.stdout);
        let output_lower = output_text.to_lowercase();

        log::info!("[CLEANUP] diskpart extend (by volume) 输出: {}", output_text);

        // 检查是否成功 - 注意要排除包含失败/错误的情况
        let has_success = output_lower.contains("成功") || output_lower.contains("successfully") 
            || output_lower.contains("extended the volume");
        let has_error = output_lower.contains("error") || output_lower.contains("错误")
            || output_lower.contains("失败") || output_lower.contains("failed")
            || output_lower.contains("没有可用") || output_lower.contains("no usable")
            || output_lower.contains("not enough") || output_lower.contains("无法");
            
        if has_success && !has_error {
            return Ok(());
        }

        // 检查是否有明确的错误：没有可用的未分配空间
        if output_lower.contains("没有可用") || output_lower.contains("no usable") 
            || output_lower.contains("not enough") || output_lower.contains("空间不足") {
            // 没有可用的未分配空间，直接失败
            anyhow::bail!("没有可用的相邻未分配空间: {}", output_text);
        }

        // 方法2：尝试通过磁盘号扩展（备用方法）
        log::info!("[CLEANUP] 尝试备用方法：通过磁盘号和分区号扩展");
        
        // 先获取分区号
        let detail = Self::get_partition_style(&format!("{}:", letter));
        if let Some(part_num) = detail.partition_number {
            let extend_script2 = format!(
                "select disk {}\nselect partition {}\nextend",
                disk_num, part_num
            );
            
            let script_path2 = temp_dir.join("lr_extend2.txt");
            std::fs::write(&script_path2, &extend_script2)?;

            let output2 = new_command(&get_diskpart_path())
                .args(["/s", script_path2.to_str().unwrap()])
                .output()?;

            let _ = std::fs::remove_file(&script_path2);

            let output_text2 = gbk_to_utf8(&output2.stdout);
            let output_lower2 = output_text2.to_lowercase();

            log::info!("[CLEANUP] diskpart extend (by partition) 输出: {}", output_text2);

            let has_success2 = output_lower2.contains("成功") || output_lower2.contains("successfully")
                || output_lower2.contains("extended the volume");
            let has_error2 = output_lower2.contains("error") || output_lower2.contains("错误")
                || output_lower2.contains("失败") || output_lower2.contains("failed")
                || output_lower2.contains("没有可用") || output_lower2.contains("no usable");
                
            if has_success2 && !has_error2 {
                return Ok(());
            }
            
            // 备用方法也失败了，返回备用方法的错误信息
            if has_error2 {
                anyhow::bail!("extend 失败 (备用方法): {}", output_text2);
            }
        }

        // 都失败了，返回第一次的错误
        if has_error {
            anyhow::bail!("extend 失败: {}", output_text);
        }

        // 不确定状态，假设失败
        anyhow::bail!("extend 状态不确定: {}", output_text)
    }
}
