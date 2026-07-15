use anyhow::Result;
use lr_core::command::{CommandExecutor, CommandOutcome, CommandRequest, SystemCommandExecutor};
use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
};

use crate::tr;
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

fn diskpart_reports_success(output: &str) -> bool {
    let output = output.to_lowercase();
    output.contains("成功")
        || output.contains("successfully")
        || output.contains("extended the volume")
}

fn diskpart_reports_no_space(output: &str) -> bool {
    let output = output.to_lowercase();
    output.contains("没有可用")
        || output.contains("没有足够")
        || output.contains("no usable")
        || output.contains("not enough")
        || output.contains("空间不足")
}

/// 分区表类型
#[derive(Debug, Clone, Copy, PartialEq, Default)]
// Keep the established names because they are shown verbatim throughout both endpoints.
#[allow(clippy::upper_case_acronyms)]
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
    fn format_failure_hint() -> String {
        tr!(
            "可能原因:\n- 目标盘质量较差或已损坏（坏盘/扩容盘/掉盘）\n- 磁盘存在坏道、I/O 错误或 CRC 错误\n- 数据线、USB 口、硬盘盒或供电不稳定\n- 分区被占用、写保护或分区表异常"
        )
    }

    fn format_partition_with_format_command(
        drive: &str,
        volume_label: &str,
    ) -> std::result::Result<String, String> {
        lr_core::format_command::validate_cmd_wrapper_label(volume_label)
            .map_err(|error| tr!("PE fallback 卷标不安全: {}", error))?;
        let spec =
            lr_core::format_command::FormatCommandSpec::new(drive, "NTFS", Some(volume_label))
                .map_err(|error| tr!("无效的格式化参数: {}", error))?
                .with_force_dismount(true);
        let drive = spec.drive().to_string();
        let args = spec.args();
        let mut cmd_args = vec!["/d", "/s", "/c", "format.com"];
        cmd_args.extend(args.iter().map(String::as_str));
        log::warn!("[FORMAT] DiskPart 失败，尝试 PE fallback: format {}", drive);

        // Keep the legacy WinPE shell wrapper: direct format.com may finish the
        // format but never exit under CREATE_NO_WINDOW on affected PE images.
        let request = CommandRequest::new("cmd").args(&cmd_args);
        let output = SystemCommandExecutor
            .execute(&request)
            .map_err(|e| tr!("执行 format 命令失败: {}", e))?;

        let stdout = gbk_to_utf8(output.stdout());
        let stderr = gbk_to_utf8(output.stderr());
        let combined = format!("{}\n{}", stdout.trim(), stderr.trim());

        log::info!("[FORMAT] fallback format stdout:\n{}", stdout);
        if !stderr.is_empty() {
            log::warn!("[FORMAT] fallback format stderr:\n{}", stderr);
        }

        if lr_core::format_command::output_indicates_success(&stdout)
            && !lr_core::format_command::output_indicates_error(
                output.succeeded(),
                &stdout,
                &stderr,
            )
        {
            log::info!("[FORMAT] fallback format {} 成功", drive);
            Ok(stdout)
        } else if !combined.trim().is_empty() {
            Err(combined.trim().to_string())
        } else {
            Err(tr!("format 命令无输出，格式化失败。"))
        }
    }

    /// 选择一个可靠的临时目录并确保它存在。
    /// WinPE 下 std::env::temp_dir() 可能指向不存在的路径，
    /// 直接写 diskpart 脚本会触发 "系统找不到指定的路径 (os error 3)"。
    fn reliable_temp_dir() -> PathBuf {
        // 统一走 system_utils::get_temp_directory（按 SystemRoot/PE系统盘动态解析，不写死 X:）
        crate::core::system_utils::get_temp_directory()
    }

    fn execute_diskpart(prefix: &str, script: &str) -> Result<CommandOutcome> {
        lr_core::diskpart::execute_script(
            &Self::reliable_temp_dir(),
            prefix,
            get_diskpart_path(),
            script,
        )
        .map_err(Into::into)
    }

    fn execute_diskpart_checked(prefix: &str, script: &str) -> Result<String> {
        lr_core::diskpart::execute_script_checked(
            &Self::reliable_temp_dir(),
            prefix,
            get_diskpart_path(),
            script,
        )
        .map_err(Into::into)
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
        let stdout = match Self::execute_diskpart_checked("lr-partition-detail", &script) {
            Ok(stdout) => stdout,
            Err(error) => {
                log::debug!("[disk] 无法查询卷 {} 的分区信息: {}", letter, error);
                return PartitionDetail {
                    style: PartitionStyle::Unknown,
                    disk_number: None,
                    partition_number: None,
                };
            }
        };

        let mut disk_num: Option<u32> = None;
        let mut part_num: Option<u32> = None;

        for line in stdout.lines() {
            let line_upper = line.to_uppercase();
            if (line_upper.contains("磁盘") || line_upper.contains("DISK"))
                && !line_upper.contains("磁盘 ID")
                && !line_upper.contains("DISK ID")
            {
                if let Some(num) = line.split_whitespace().find(|s| s.parse::<u32>().is_ok()) {
                    disk_num = num.parse().ok();
                }
            }
            if line_upper.contains("分区") || line_upper.contains("PARTITION") {
                if let Some(num) = line.split_whitespace().find(|s| s.parse::<u32>().is_ok()) {
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
        // `detail disk` does not consistently print the words GPT/MBR across
        // DiskPart versions and languages. `uniqueid disk` is stable: GPT uses
        // a GUID and MBR uses an eight-digit hexadecimal disk signature.
        let script = format!("select disk {}\nuniqueid disk", disk_number);
        match Self::execute_diskpart_checked("lr-disk-style", &script) {
            Ok(stdout) => Self::partition_style_from_unique_id_output(&stdout),
            Err(error) => {
                log::debug!(
                    "[disk] 无法查询磁盘 {} 的分区表类型: {}",
                    disk_number,
                    error
                );
                PartitionStyle::Unknown
            }
        }
    }

    fn partition_style_from_unique_id_output(output: &str) -> PartitionStyle {
        for raw in output.split_whitespace() {
            let value = raw
                .trim_matches(|character: char| matches!(character, '{' | '}' | ':' | ',' | ';'));
            if value.len() == 36
                && value
                    .chars()
                    .enumerate()
                    .all(|(index, character)| match index {
                        8 | 13 | 18 | 23 => character == '-',
                        _ => character.is_ascii_hexdigit(),
                    })
            {
                return PartitionStyle::GPT;
            }
            if value.len() == 8 && value.chars().all(|character| character.is_ascii_hexdigit()) {
                return PartitionStyle::MBR;
            }
        }
        PartitionStyle::Unknown
    }

    /// 格式化指定分区（带卷标）
    ///
    /// 使用 cmd /c format 进行格式化，因为直接调用 format.com 在 CREATE_NO_WINDOW 模式下
    /// 会完成格式化但进程不退出，导致程序卡死。通过 cmd /c 包装可以正常退出。
    pub fn format_partition_with_label(
        partition: &str,
        volume_label: Option<&str>,
    ) -> Result<String> {
        log::info!("格式化分区: {} 卷标: {:?}", partition, volume_label);

        let vol_label = match volume_label {
            Some(label) if !label.is_empty() => label,
            _ => "本地磁盘",
        };
        let spec =
            lr_core::format_command::FormatCommandSpec::new(partition, "NTFS", Some(vol_label))
                .map_err(|error| anyhow::anyhow!("无效的格式化参数: {error}"))?;
        let drive = spec.drive().to_string();
        let drive_letter = drive.as_bytes()[0] as char;
        let vol_label = spec.volume_label().unwrap_or("本地磁盘");

        // 使用 diskpart 格式化，避免 format.com 在 PE 中的交互和参数兼容问题
        let script = format!(
            "select volume {}\r\nformat fs=ntfs label=\"{}\" quick override\r\nexit\r\n",
            drive_letter, vol_label
        );
        let cmd_args = format!("diskpart format {}", drive);

        log::info!("执行命令: {}", cmd_args);

        let temp_dir = Self::reliable_temp_dir();
        let script_file = lr_core::scoped_temp_file::ScopedTempFile::create_in(
            &temp_dir,
            "lr_format_part",
            "txt",
            script.as_bytes(),
        )?;
        let script_path_str = script_file
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("temporary diskpart script path is not UTF-8"))?;

        let diskpart_path = get_diskpart_path();

        let request = CommandRequest::new(&diskpart_path).args(["/s", script_path_str]);
        let output = SystemCommandExecutor.execute(&request)?;

        let stdout = gbk_to_utf8(output.stdout());
        let stderr = gbk_to_utf8(output.stderr());

        log::info!("format 输出:\n{}", stdout);
        if !stderr.is_empty() {
            log::warn!("format 错误输出:\n{}", stderr);
        }

        // DiskPart 会输出格式化进度，例如“0 百分比已完成”/“0 percent completed”。
        // 这些进度行不能作为成功依据；只接受明确的成功句。
        let stdout_lower = stdout.to_lowercase();
        let combined = format!("{}\n{}", stdout.trim(), stderr.trim());
        let combined_lower = combined.to_lowercase();
        let has_success_indicator = stdout.contains("\u{6210}\u{529f}\u{683c}\u{5f0f}\u{5316}")
            || stdout_lower.contains("successfully formatted");
        let has_error_indicator = !output.succeeded()
            || stdout.contains("无法")
            || stdout.contains("错误")
            || stdout.contains("失败")
            || stdout.contains("拒绝")
            || stderr.contains("无法")
            || stderr.contains("错误")
            || stderr.contains("失败")
            || stderr.contains("拒绝")
            || combined_lower.contains("diskpart has encountered an error")
            || combined_lower.contains("error")
            || combined_lower.contains("failed")
            || combined_lower.contains("denied")
            || combined_lower.contains("i/o device error")
            || combined_lower.contains("cyclic redundancy check");

        if has_success_indicator && !has_error_indicator {
            log::info!("分区 {} 格式化成功", drive);
            Ok(stdout)
        } else {
            let diskpart_detail = if !combined.trim().is_empty() {
                combined.trim().to_string()
            } else {
                tr!("diskpart 无输出，格式化失败。请确认该分区未被占用后重试。")
            };
            match Self::format_partition_with_format_command(&drive, vol_label) {
                Ok(format_stdout) => {
                    log::info!("[FORMAT] DiskPart 失败后 fallback format 成功");
                    Ok(format!("{}\n{}", stdout.trim(), format_stdout.trim()))
                }
                Err(format_detail) => {
                    log::warn!("[FORMAT] fallback format 也失败: {}", format_detail);
                    let hint = Self::format_failure_hint();
                    let error_msg = if output.succeeded() {
                        tr!(
                            "格式化失败。\n{}\n\nDiskPart 输出:\n{}\n\nformat 输出:\n{}",
                            hint,
                            diskpart_detail,
                            format_detail
                        )
                    } else {
                        tr!(
                            "格式化失败，diskpart 退出码 {}。\n{}\n\nDiskPart 输出:\n{}\n\nformat 输出:\n{}",
                            output
                                .exit_code()
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| tr!("未知")),
                            hint,
                            diskpart_detail,
                            format_detail
                        )
                    };

                    log::error!("格式化失败: {}", error_msg);
                    anyhow::bail!("{}", error_msg);
                }
            }
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

    /// Resolve the install boot mode against the selected target disk.
    ///
    /// Auto must follow the target partition table rather than the way WinPE
    /// itself was booted. The PE firmware mode is only a last-resort fallback
    /// when DiskPart cannot identify the target disk layout.
    pub fn resolve_install_uefi_mode(boot_mode: u8, target_partition: &str) -> bool {
        match boot_mode {
            1 => true,
            2 => false,
            _ => {
                let detail = Self::get_partition_style(target_partition);
                match detail.style {
                    PartitionStyle::GPT => {
                        log::info!(
                            "[BOOT] 自动模式：目标分区 {} 位于 GPT 磁盘，使用 UEFI",
                            target_partition
                        );
                        true
                    }
                    PartitionStyle::MBR => {
                        log::info!(
                            "[BOOT] 自动模式：目标分区 {} 位于 MBR 磁盘，使用 Legacy",
                            target_partition
                        );
                        false
                    }
                    PartitionStyle::Unknown => {
                        let fallback = Self::detect_uefi_mode();
                        log::warn!(
                            "[BOOT] 无法识别目标分区 {} 的分区表，回退当前 PE 固件模式: {}",
                            target_partition,
                            if fallback { "UEFI" } else { "Legacy" }
                        );
                        fallback
                    }
                }
            }
        }
    }

    fn read_auto_marker_source(letter: char) -> Option<char> {
        let marker_path = format!("{}:\\{}", letter, AUTO_CREATED_PARTITION_MARKER);
        let content = std::fs::read_to_string(marker_path).ok()?;
        for line in content.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("Source:") {
                return value
                    .trim()
                    .chars()
                    .find(|c| c.is_ascii_alphabetic())
                    .map(|c| c.to_ascii_uppercase());
            }
            if let Some(value) = line.strip_prefix("Source=") {
                return value
                    .trim()
                    .chars()
                    .find(|c| c.is_ascii_alphabetic())
                    .map(|c| c.to_ascii_uppercase());
            }
        }
        None
    }

    /// 查找自动创建的分区（通过标志文件）
    /// 返回 (盘符, 磁盘号Option, 来源盘符Option) 如果找到的话
    pub fn find_auto_created_partition() -> Option<(char, Option<u32>, Option<char>)> {
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
                return Some((c, detail.disk_number, Self::read_auto_marker_source(c)));
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
        let target_letter = target_partition
            .chars()
            .next()
            .unwrap_or('C')
            .to_ascii_uppercase();

        log::info!("[CLEANUP] ========================================");
        log::info!("[CLEANUP] 开始清理自动创建的分区");
        log::info!("[CLEANUP] 目标安装分区: {}:", target_letter);
        log::info!("[CLEANUP] ========================================");

        // 查找自动创建的分区
        let (auto_letter, auto_disk_num_opt, marker_source) =
            match Self::find_auto_created_partition() {
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
                anyhow::bail!(
                    "[CLEANUP] 无法获取自动创建分区 {} 的磁盘号，已取消删除",
                    auto_letter
                );
            }
        };
        let auto_part_num = auto_detail.partition_number;

        log::info!(
            "[CLEANUP] 找到自动创建的分区: {}:, 磁盘 {}, 分区号 {:?}",
            auto_letter,
            auto_disk_num,
            auto_part_num
        );

        // 获取目标分区所在的磁盘号和分区号
        let target_detail = Self::get_partition_style(&format!("{}:", target_letter));
        let target_disk_num = match target_detail.disk_number {
            Some(num) => num,
            None => {
                anyhow::bail!(
                    "[CLEANUP] 无法获取目标分区 {} 的磁盘号，已取消删除自动创建分区",
                    target_letter
                );
            }
        };
        let target_part_num = target_detail.partition_number;

        log::info!(
            "[CLEANUP] 目标分区: {}:, 磁盘 {}, 分区号 {:?}",
            target_letter,
            target_disk_num,
            target_part_num
        );

        let source_letter = marker_source.ok_or_else(|| {
            anyhow::anyhow!(
                "[CLEANUP] 自动创建分区 {} 的标记缺少 Source，无法确认归属，已取消删除",
                auto_letter
            )
        })?;
        if source_letter != target_letter {
            anyhow::bail!(
                "[CLEANUP] 自动创建分区 {} 的 Source 为 {}:，与目标分区 {}: 不一致，已取消删除",
                auto_letter,
                source_letter,
                target_letter
            );
        }

        // 检查是否在同一磁盘
        if auto_disk_num != target_disk_num {
            anyhow::bail!(
                "[CLEANUP] 自动创建的分区 (磁盘{}) 和目标分区 (磁盘{}) 不在同一磁盘，已取消删除",
                auto_disk_num,
                target_disk_num
            );
        }

        // 检查分区相邻性：临时分区应该在目标分区之后
        // diskpart extend 只能向后扩展到相邻的未分配空间
        let target_pn = target_part_num.ok_or_else(|| {
            anyhow::anyhow!("[CLEANUP] 无法获取目标分区号，已取消删除自动创建分区")
        })?;
        let auto_pn = auto_part_num
            .ok_or_else(|| anyhow::anyhow!("[CLEANUP] 无法获取自动创建分区号，已取消删除"))?;

        if auto_pn != target_pn + 1 {
            anyhow::bail!(
                "[CLEANUP] 自动创建分区 (分区号{}) 不是目标分区 (分区号{}) 的紧邻后方分区，已取消删除",
                auto_pn,
                target_pn
            );
        }
        log::info!(
            "[CLEANUP] 分区相邻性检查通过：目标分区{} -> 临时分区{}",
            target_pn,
            auto_pn
        );

        // 删除自动创建分区并扩展目标分区
        log::info!(
            "[CLEANUP] 开始删除分区 {} 并扩展目标分区 {}...",
            auto_letter,
            target_letter
        );
        Self::delete_partition_and_extend(auto_letter, target_letter, auto_disk_num)
    }

    /// 删除指定盘符的分区
    #[allow(
        dead_code,
        reason = "retained as a compatibility fallback for PE cleanup flows"
    )]
    fn delete_partition_by_letter(letter: char) -> Result<()> {
        log::info!("[CLEANUP] 删除分区 {}:", letter);

        let script_content = format!("select volume {}\ndelete partition override", letter);
        let output_text = Self::execute_diskpart_checked("lr-delete-partition", &script_content)?;
        log::info!("[CLEANUP] Diskpart 删除输出: {}", output_text);

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
    fn delete_partition_and_extend(
        auto_letter: char,
        target_letter: char,
        disk_num: u32,
    ) -> Result<()> {
        // 记录扩展前的分区大小
        let size_before = Self::get_partition_size_mb(target_letter);
        log::info!("[CLEANUP] 扩展前目标分区大小: {:?} MB", size_before);

        // Step 1: 删除分区
        log::info!("[CLEANUP] Step 1: 删除分区 {}:", auto_letter);

        let delete_script = format!("select volume {}\ndelete partition override", auto_letter);
        let output_text = Self::execute_diskpart_checked("lr-delete-and-extend", &delete_script)?;
        log::info!("[CLEANUP] 删除分区输出: {}", output_text);

        log::info!("[CLEANUP] 分区 {} 删除成功", auto_letter);

        // Step 2: 运行 rescan 命令刷新磁盘信息
        log::info!("[CLEANUP] Step 2: 刷新磁盘信息 (rescan)");
        Self::diskpart_rescan();

        // 等待系统处理 rescan
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Step 3: 等待系统识别未分配空间，然后扩展目标分区（带重试）
        log::info!("[CLEANUP] Step 3: 扩展目标分区 {}（带重试）", target_letter);

        const MAX_RETRIES: u32 = 10; // 增加到 10 次
        const RETRY_DELAY_SECS: u64 = 3; // 增加到 3 秒

        let mut last_error = String::new();

        for attempt in 1..=MAX_RETRIES {
            log::info!(
                "[CLEANUP] 扩展分区 {} 尝试 {}/{}",
                target_letter,
                attempt,
                MAX_RETRIES
            );

            // 尝试扩展
            match Self::try_extend_volume_enhanced(target_letter, disk_num) {
                Ok(_) => {
                    // 验证扩展是否成功
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let size_after = Self::get_partition_size_mb(target_letter);
                    log::info!("[CLEANUP] 扩展后目标分区大小: {:?} MB", size_after);

                    if let (Some(before), Some(after)) = (size_before, size_after) {
                        if after > before {
                            log::info!(
                                "[CLEANUP] 分区 {} 扩展成功！大小从 {} MB 增加到 {} MB",
                                target_letter,
                                before,
                                after
                            );
                            return Ok(());
                        } else {
                            // extend 命令返回成功但分区大小未变化
                            // 可能是系统还未识别到未分配空间，继续重试
                            last_error = format!(
                                "extend 命令执行成功但分区大小未变化 (before={} MB, after={} MB)",
                                before, after
                            );
                            log::warn!("[CLEANUP] {}", last_error);
                        }
                    } else {
                        // 无法获取大小进行比较，假设成功
                        log::info!(
                            "[CLEANUP] 分区 {} 扩展命令执行成功（无法验证大小变化）",
                            target_letter
                        );
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
        anyhow::bail!("数据分区已删除，但扩展目标分区失败: {}", last_error)
    }

    /// 运行 diskpart rescan 命令刷新磁盘信息
    fn diskpart_rescan() {
        match Self::execute_diskpart_checked("lr-rescan", "rescan") {
            Ok(output) => log::info!("[CLEANUP] rescan 输出: {}", output),
            Err(error) => {
                // rescan is advisory here; the subsequent extend loop still
                // performs its own retries and verification.
                log::warn!("[CLEANUP] rescan 失败，将继续重试扩展: {}", error);
            }
        }
    }

    /// 尝试扩展指定分区（增强版，使用 diskpart）
    /// 先尝试通过卷字母扩展，如果失败则尝试通过磁盘号和分区号扩展
    fn try_extend_volume_enhanced(letter: char, disk_num: u32) -> Result<()> {
        // 方法1：通过卷字母扩展（标准方法）
        let extend_script = format!("select volume {}\nextend", letter);
        let output = Self::execute_diskpart("lr-extend-volume", &extend_script)?;
        let validation = lr_core::diskpart::validated_stdout(&output);
        let output_text = match &validation {
            Ok(text) | Err(text) => text,
        };

        log::info!(
            "[CLEANUP] diskpart extend (by volume) 输出: {}",
            output_text
        );

        if validation.is_ok() && diskpart_reports_success(output_text) {
            return Ok(());
        }

        // 检查是否有明确的错误：没有可用的未分配空间
        if diskpart_reports_no_space(output_text) {
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
            let output2 = Self::execute_diskpart("lr-extend-partition", &extend_script2)?;
            let validation2 = lr_core::diskpart::validated_stdout(&output2);
            let output_text2 = match &validation2 {
                Ok(text) | Err(text) => text,
            };

            log::info!(
                "[CLEANUP] diskpart extend (by partition) 输出: {}",
                output_text2
            );

            if validation2.is_ok() && diskpart_reports_success(output_text2) {
                return Ok(());
            }

            anyhow::bail!("extend 失败 (备用方法): {}", output_text2);
        }

        anyhow::bail!("extend 失败: {}", output_text)
    }

    /// 无损扩大分区到指定大小（仅并入紧邻其后的未分配空间；不移动其它分区）。
    ///
    /// - `letter`：目标分区盘符（如 'C'）。在 PE 下应由扩容标记定位后传入。
    /// - `target_size_mb`：期望最终总大小（MB）；0 = 尽可能扩到最大（吃光相邻未分配空间）。
    ///
    /// 实现：diskpart `select volume L` + `extend [size=delta]`。`extend` 只能并入紧跟该
    /// 卷之后的未分配空间——这是无损、安全的操作。若其后是别的分区(需要分区移动)，diskpart
    /// 会报“没有可用的未分配空间”，本函数据此返回明确错误（分区移动属另一条尚未启用的路径）。
    pub fn expand_partition_lossless(letter: char, target_size_mb: u64) -> Result<String> {
        let current_mb = Self::get_partition_size_mb(letter)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法获取分区 {}: 的当前大小", letter)))?;
        log::info!(
            "[EXPAND] 目标分区 {}: 当前 {} MB，目标 {} MB",
            letter,
            current_mb,
            target_size_mb
        );

        // 计算 extend 的 size 参数（MB）。0 或不大于当前 → 扩到最大（不带 size）。
        let size_arg = if target_size_mb == 0 || target_size_mb <= current_mb {
            None
        } else {
            Some(target_size_mb - current_mb)
        };

        let script = match size_arg {
            Some(delta) => format!("select volume {}\r\nextend size={}\r\n", letter, delta),
            None => format!("select volume {}\r\nextend\r\n", letter),
        };

        let output = Self::execute_diskpart("lr-expand-lossless", &script)?;
        let validation = lr_core::diskpart::validated_stdout(&output);
        let text = match &validation {
            Ok(text) | Err(text) => text,
        };
        log::info!("[EXPAND] diskpart 输出: {}", text);

        if diskpart_reports_no_space(text) {
            anyhow::bail!(
                "{}",
                tr!("C 盘后面没有相邻的未分配空间可并入。若要从后面的分区夺取空间，需要分区移动功能（暂未启用）。")
            );
        }
        if validation.is_err() || !diskpart_reports_success(text) {
            anyhow::bail!("{}", tr!("扩容失败: {}", text));
        }

        let new_mb = Self::get_partition_size_mb(letter).unwrap_or(current_mb);
        if new_mb <= current_mb {
            anyhow::bail!(
                "{}",
                tr!(
                    "diskpart 报告成功，但分区大小未增加（{} MB）。可能没有相邻未分配空间。",
                    new_mb
                )
            );
        }
        Ok(tr!(
            "分区 {}: 已从 {} MB 扩大到 {} MB",
            letter,
            current_mb,
            new_mb
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{diskpart_reports_no_space, diskpart_reports_success, DiskManager, PartitionStyle};

    #[test]
    fn parses_diskpart_unique_ids_without_depending_on_language() {
        assert_eq!(
            DiskManager::partition_style_from_unique_id_output(
                "Disk ID: {01234567-89AB-CDEF-0123-456789ABCDEF}"
            ),
            PartitionStyle::GPT
        );
        assert_eq!(
            DiskManager::partition_style_from_unique_id_output("磁盘 ID: 89ABCDEF"),
            PartitionStyle::MBR
        );
        assert_eq!(
            DiskManager::partition_style_from_unique_id_output("DiskPart failed"),
            PartitionStyle::Unknown
        );
    }

    #[test]
    fn classifies_localized_extend_results() {
        assert!(diskpart_reports_success(
            "DiskPart successfully extended the volume."
        ));
        assert!(diskpart_reports_success("DiskPart 成功扩展了卷。"));
        assert!(diskpart_reports_no_space(
            "There is not enough usable free space on specified disk(s)."
        ));
        assert!(diskpart_reports_no_space(
            "没有足够的可用空间来执行此操作。"
        ));
        assert!(!diskpart_reports_success(
            "DiskPart failed to extend the volume."
        ));
    }
}
