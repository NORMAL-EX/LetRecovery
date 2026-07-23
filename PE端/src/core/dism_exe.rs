//! DISM.exe 命令行封装模块
//!
//! 该模块使用 PE 环境自带的 dism.exe 命令行工具实现：
//! - 离线驱动导入
//! - 离线 Windows Update CAB 包安装
//!
//! 相比 DISM API 或 WinAPI，直接调用 dism.exe 在 PE 环境下更加可靠稳定。
//! dism.exe 位于 PE 环境的 X:\Windows\System32\dism.exe

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;

use anyhow::{bail, Context, Result};
use lr_core::registry::OfflineRegistry;

#[cfg(windows)]
use windows::Win32::Globalization::LCIDToLocaleName;

use crate::tr;
use crate::utils::encoding::gbk_to_utf8;

/// Windows CREATE_NO_WINDOW 标志，用于隐藏控制台窗口
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

static OFFLINE_INTL_HIVE_SEQUENCE: AtomicU32 = AtomicU32::new(1);

/// DISM 操作进度
#[derive(Debug, Clone)]
pub struct DismExeProgress {
    pub percentage: u8,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineInternationalSettings {
    pub ui_language: String,
    pub system_locale: String,
    pub user_locale: String,
    pub input_locale: String,
    pub time_zone: String,
}

fn field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    name.trim()
        .eq_ignore_ascii_case(field)
        .then_some(value.trim())
        .filter(|value| !value.is_empty())
}

fn valid_locale_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 35
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn valid_input_locale(value: &str) -> bool {
    let Some((language, keyboard)) = value.split_once(':') else {
        return valid_locale_name(value);
    };
    language.len() == 4
        && keyboard.len() == 8
        && language
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        && keyboard
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn valid_time_zone(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value
            .chars()
            .any(|character| matches!(character, '<' | '>'))
}

fn locale_id_from_registry(value: &str) -> Result<u32> {
    let normalized = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or_else(|| value.trim());
    if normalized.is_empty()
        || normalized.len() > 8
        || !normalized
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("无效的十六进制区域标识: {value}");
    }
    u32::from_str_radix(normalized, 16).with_context(|| format!("无法解析区域标识: {value}"))
}

#[cfg(windows)]
fn locale_name_from_registry_id(value: &str) -> Result<String> {
    let locale_id = locale_id_from_registry(value)?;
    let mut buffer = [0u16; 85];
    let length = unsafe { LCIDToLocaleName(locale_id, Some(&mut buffer), 0) };
    if length == 0 {
        bail!("Windows 无法把区域标识 {value} 转换为区域名称");
    }
    let locale_name = String::from_utf16(&buffer[..length.saturating_sub(1) as usize])
        .context("Windows 返回了无效的 UTF-16 区域名称")?;
    if !valid_locale_name(&locale_name) {
        bail!("Windows 返回了无效的区域名称: {locale_name}");
    }
    Ok(locale_name)
}

#[cfg(not(windows))]
fn locale_name_from_registry_id(value: &str) -> Result<String> {
    let _ = locale_id_from_registry(value)?;
    bail!("离线 Windows 区域标识转换只能在 Windows 上执行")
}

fn input_locale_from_keyboard_layout(value: &str) -> Result<String> {
    let keyboard_layout = value.trim();
    if keyboard_layout.len() != 8
        || !keyboard_layout
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("无效的默认键盘布局: {value}");
    }
    let language_id = &keyboard_layout[4..];
    let input_locale = format!("{language_id}:{keyboard_layout}");
    if !valid_input_locale(&input_locale) {
        bail!("无法从默认键盘布局构造输入区域: {value}");
    }
    Ok(input_locale)
}

struct LoadedOfflineHive {
    name: String,
}

impl LoadedOfflineHive {
    fn load(name: String, hive_file: &Path) -> Result<Self> {
        let hive_file = hive_file.to_str().ok_or_else(|| {
            anyhow::anyhow!("离线注册表路径不是有效的 Unicode: {}", hive_file.display())
        })?;
        OfflineRegistry::load_hive(&name, hive_file)
            .with_context(|| format!("加载离线注册表配置单元失败: {hive_file}"))?;
        Ok(Self { name })
    }

    fn key(&self, relative_path: &str) -> String {
        format!("HKLM\\{}\\{}", self.name, relative_path)
    }
}

impl Drop for LoadedOfflineHive {
    fn drop(&mut self) {
        if let Err(error) = OfflineRegistry::unload_hive(&self.name) {
            log::error!(
                "[UNATTEND] 卸载国际化探测注册表配置单元失败 [{}]: {:#}",
                self.name,
                error
            );
        }
    }
}

fn read_offline_international_settings_from_registry(
    image_path: &str,
) -> Result<OfflineInternationalSettings> {
    let image_root = image_path.trim_end_matches(['\\', '/']);
    if image_root.len() != 2
        || !image_root.as_bytes()[0].is_ascii_alphabetic()
        || image_root.as_bytes()[1] != b':'
    {
        bail!("离线系统根目录必须是盘符: {image_path}");
    }

    let config_dir = PathBuf::from(format!(r"{}\Windows\System32\config", image_root));
    let system_hive_path = config_dir.join("SYSTEM");
    let default_hive_path = config_dir.join("DEFAULT");
    if !system_hive_path.is_file() || !default_hive_path.is_file() {
        bail!(
            "目标系统缺少国际化探测所需的 SYSTEM 或 DEFAULT 注册表配置单元: {}",
            config_dir.display()
        );
    }

    let sequence = OFFLINE_INTL_HIVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let prefix = format!("lr-intl-{}-{sequence}", std::process::id());
    let system_hive = LoadedOfflineHive::load(format!("{prefix}-system"), &system_hive_path)?;
    let default_hive = LoadedOfflineHive::load(format!("{prefix}-default"), &default_hive_path)?;

    let select_key = system_hive.key("Select");
    let control_set =
        OfflineRegistry::query_dword(&select_key, "Current").or_else(|current_error| {
            OfflineRegistry::query_dword(&select_key, "Default").with_context(|| {
                format!("读取活动控制集 Current 失败 ({current_error:#})，且 Default 回退也失败")
            })
        })?;
    if !(1..=999).contains(&control_set) {
        bail!("离线 SYSTEM 注册表返回了无效的控制集编号: {control_set}");
    }
    let control_root = format!("ControlSet{control_set:03}\\Control");

    let language_key = system_hive.key(&format!(r"{control_root}\Nls\Language"));
    let install_language = OfflineRegistry::query_string(&language_key, "InstallLanguage")
        .context("读取目标系统安装语言失败")?;
    let system_language = OfflineRegistry::query_string(&language_key, "Default")
        .context("读取目标系统区域设置失败")?;
    let ui_language =
        locale_name_from_registry_id(&install_language).context("转换目标系统安装语言失败")?;
    let system_locale =
        locale_name_from_registry_id(&system_language).context("转换目标系统区域设置失败")?;

    let international_key = default_hive.key(r"Control Panel\International");
    let user_locale = OfflineRegistry::query_string(&international_key, "LocaleName")
        .context("读取目标系统默认用户区域设置失败")?;
    if !valid_locale_name(&user_locale) {
        bail!("离线 DEFAULT 注册表返回了无效的用户区域设置: {user_locale}");
    }

    let keyboard_key = default_hive.key(r"Keyboard Layout\Preload");
    let keyboard_layout = OfflineRegistry::query_string(&keyboard_key, "1")
        .context("读取目标系统默认键盘布局失败")?;
    let input_locale = input_locale_from_keyboard_layout(&keyboard_layout)?;

    let time_zone_key = system_hive.key(&format!(r"{control_root}\TimeZoneInformation"));
    let time_zone = OfflineRegistry::query_string(&time_zone_key, "TimeZoneKeyName")
        .context("读取目标系统默认时区失败")?;
    if !valid_time_zone(&time_zone) {
        bail!("离线 SYSTEM 注册表返回了无效的默认时区: {time_zone}");
    }

    Ok(OfflineInternationalSettings {
        ui_language,
        system_locale,
        user_locale,
        input_locale,
        time_zone,
    })
}

fn parse_offline_international_settings(output: &str) -> Result<OfflineInternationalSettings> {
    let mut ui_language = None;
    let mut system_locale = None;
    let mut user_locale = None;
    let mut input_locale = None;
    let mut time_zone = None;

    for line in output.lines() {
        ui_language = ui_language
            .or_else(|| field_value(line, "Default system UI language").map(str::to_string));
        system_locale =
            system_locale.or_else(|| field_value(line, "System locale").map(str::to_string));
        user_locale = user_locale.or_else(|| field_value(line, "User locale").map(str::to_string));
        time_zone =
            time_zone.or_else(|| field_value(line, "Default time zone").map(str::to_string));
        if input_locale.is_none() {
            input_locale = field_value(line, "Active keyboard(s)")
                .and_then(|value| value.split([',', ';', ' ']).find(|item| !item.is_empty()))
                .map(str::to_string);
        }
    }

    let ui_language = ui_language
        .filter(|value| valid_locale_name(value))
        .ok_or_else(|| anyhow::anyhow!("DISM /Get-Intl 未返回有效的默认系统 UI 语言"))?;
    let system_locale = system_locale
        .filter(|value| valid_locale_name(value))
        .ok_or_else(|| anyhow::anyhow!("DISM /Get-Intl 未返回有效的系统区域设置"))?;
    let user_locale = user_locale.unwrap_or_else(|| ui_language.clone());
    if !valid_locale_name(&user_locale) {
        anyhow::bail!("DISM /Get-Intl 返回了无效的用户区域设置: {user_locale}");
    }
    let input_locale = input_locale
        .filter(|value| valid_input_locale(value))
        .ok_or_else(|| anyhow::anyhow!("DISM /Get-Intl 未返回有效的活动键盘布局"))?;
    let time_zone = time_zone
        .filter(|value| valid_time_zone(value))
        .ok_or_else(|| anyhow::anyhow!("DISM /Get-Intl 未返回有效的默认时区"))?;

    Ok(OfflineInternationalSettings {
        ui_language,
        system_locale,
        user_locale,
        input_locale,
        time_zone,
    })
}

/// DISM.exe 执行器
///
/// 封装了使用 dism.exe 命令行工具进行离线镜像服务的所有操作。
/// 自动定位 PE 环境中的 dism.exe 并使用隐藏窗口模式执行。
pub struct DismExe {
    dism_path: PathBuf,
}

impl DismExe {
    /// 创建 DismExe 实例
    ///
    /// 自动查找 PE 环境或系统中可用的 dism.exe
    pub fn new() -> Result<Self> {
        let dism_path = Self::find_dism_exe()?;
        log::info!("[DISM.EXE] 使用 dism.exe: {}", dism_path.display());
        Ok(Self { dism_path })
    }

    /// 查找可用的 dism.exe
    ///
    /// 按照优先级查找：
    /// 1. PE 环境 (X:\Windows\System32\dism.exe)
    /// 2. 系统目录 (C:\Windows\System32\dism.exe)
    /// 3. PATH 环境变量
    fn find_dism_exe() -> Result<PathBuf> {
        // PE 环境路径（优先使用）
        let pe_paths = [
            PathBuf::from(r"X:\Windows\System32\dism.exe"),
            PathBuf::from(r"X:\Windows\System32\Dism\dism.exe"),
        ];

        for path in &pe_paths {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        // 尝试检测 PE 环境的系统盘符
        for letter in ['X', 'Y', 'Z', 'W'] {
            let path = PathBuf::from(format!(r"{}:\Windows\System32\dism.exe", letter));
            if path.exists() {
                return Ok(path);
            }
        }

        // 系统目录路径
        if let Ok(system_root) = std::env::var("SystemRoot") {
            let system_path = PathBuf::from(&system_root)
                .join("System32")
                .join("dism.exe");
            if system_path.exists() {
                return Ok(system_path);
            }
        }

        // 常见系统路径
        let system_paths = [
            PathBuf::from(r"C:\Windows\System32\dism.exe"),
            PathBuf::from(r"C:\Windows\System32\Dism\dism.exe"),
        ];

        for path in &system_paths {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        // 最后尝试通过 PATH 查找（使用隐藏窗口）
        let where_result = {
            let mut cmd = Command::new("where");
            cmd.arg("dism.exe");

            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }

            cmd.output()
        };

        if let Ok(output) = where_result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                let path = PathBuf::from(first_line.trim());
                if path.exists() {
                    return Ok(path);
                }
            }
        }

        bail!(
            "{}",
            tr!(
                "无法找到 dism.exe。请确保在 PE 环境或 Windows 系统中运行。\n\
             已搜索的路径:\n\
             - X:\\Windows\\System32\\dism.exe (PE 环境)\n\
             - C:\\Windows\\System32\\dism.exe (Windows 系统)"
            )
        )
    }

    /// 创建隐藏窗口的 dism.exe 命令
    fn create_command(&self) -> Command {
        let mut cmd = Command::new(&self.dism_path);

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        cmd
    }

    /// 确保临时目录存在并返回路径
    ///
    /// 在 PE 环境中优先使用 X:\Windows\TEMP，
    /// 如果不存在则尝试创建或使用其他可用的临时目录。
    fn ensure_scratch_directory() -> String {
        // 可能的临时目录列表（按优先级排序）
        let candidates = [
            r"X:\Windows\TEMP",
            r"X:\TEMP",
            r"Y:\Windows\TEMP",
            r"Y:\TEMP",
        ];

        // 尝试使用或创建候选目录
        for dir in &candidates {
            let path = Path::new(dir);
            if path.exists() {
                log::debug!("[DISM.EXE] 使用临时目录: {}", dir);
                return dir.to_string();
            }

            // 尝试创建目录
            if std::fs::create_dir_all(path).is_ok() {
                log::info!("[DISM.EXE] 创建临时目录: {}", dir);
                return dir.to_string();
            }
        }

        // 如果所有候选都失败，使用系统临时目录
        let system_temp = std::env::temp_dir();
        let temp_str = system_temp.to_string_lossy().to_string();
        log::warn!("[DISM.EXE] 使用系统临时目录: {}", temp_str);

        // 确保系统临时目录存在
        let _ = std::fs::create_dir_all(&system_temp);
        temp_str
    }

    /// 执行 DISM 命令并实时解析进度
    ///
    /// # 参数
    /// - `args`: DISM 命令行参数
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 返回
    /// - Ok(output_text) 执行成功，返回完整输出
    /// - Err(...) 执行失败
    fn execute_with_progress(
        &self,
        args: &[&str],
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<String> {
        log::info!(
            "[DISM.EXE] 执行: {} {}",
            self.dism_path.display(),
            args.join(" ")
        );

        let mut child = self
            .create_command()
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(tr!("启动 dism.exe 失败"))?;

        let stdout = child.stdout.take().context(tr!("无法获取 stdout"))?;
        let stderr = child.stderr.take().context(tr!("无法获取 stderr"))?;

        // 读取并解析 stdout
        let progress_tx_clone = progress_tx.clone();
        let stdout_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut output = String::new();
            let mut bytes = Vec::new();

            while reader.read_until(b'\n', &mut bytes).unwrap_or(0) != 0 {
                while matches!(bytes.last(), Some(b'\r' | b'\n')) {
                    bytes.pop();
                }
                let decoded_line = gbk_to_utf8(&bytes);
                bytes.clear();

                output.push_str(&decoded_line);
                output.push('\n');

                // 解析进度信息
                if let Some(ref tx) = progress_tx_clone {
                    if let Some(progress) = Self::parse_progress_line(&decoded_line) {
                        let _ = tx.send(progress);
                    }
                }

                log::trace!("[DISM.EXE STDOUT] {}", decoded_line);
            }

            output
        });

        // 读取 stderr
        let stderr_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut error_output = String::new();
            let mut bytes = Vec::new();

            while reader.read_until(b'\n', &mut bytes).unwrap_or(0) != 0 {
                while matches!(bytes.last(), Some(b'\r' | b'\n')) {
                    bytes.pop();
                }
                let decoded_line = gbk_to_utf8(&bytes);
                bytes.clear();

                error_output.push_str(&decoded_line);
                error_output.push('\n');

                log::trace!("[DISM.EXE STDERR] {}", decoded_line);
            }

            error_output
        });

        // 等待进程完成
        let status = child.wait().context(tr!("等待 dism.exe 完成失败"))?;

        // 获取输出
        let stdout_text = stdout_handle.join().unwrap_or_default();
        let stderr_text = stderr_handle.join().unwrap_or_default();

        // 发送完成进度
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismExeProgress {
                percentage: 100,
                status: tr!("完成"),
            });
        }

        if !status.success() {
            let error_msg = if !stderr_text.trim().is_empty() {
                stderr_text.trim().to_string()
            } else if !stdout_text.trim().is_empty() {
                // DISM 有时会将错误信息输出到 stdout
                Self::extract_error_from_output(&stdout_text)
            } else {
                tr!("dism.exe 退出码: {}", format!("{:?}", status.code()))
            };

            bail!("{}", tr!("DISM 操作失败: {}", error_msg));
        }

        log::info!("[DISM.EXE] 操作成功完成");
        Ok(stdout_text)
    }

    /// 只读查询已经释放到目标分区的 Windows 国际化默认值。
    /// 这些值必须写入 oobeSystem，避免 Windows 11 因语言或键盘仍待确认而重新进入用户 OOBE。
    pub fn get_offline_international_settings(
        &self,
        image_path: &str,
    ) -> Result<OfflineInternationalSettings> {
        let normalized_image = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };
        let image_arg = format!("/Image:{normalized_image}");
        let dism_result = self
            .execute_with_progress(&["/English", &image_arg, "/Get-Intl"], None)
            .and_then(|output| parse_offline_international_settings(&output));
        match dism_result {
            Ok(settings) => Ok(settings),
            Err(dism_error) => {
                log::warn!(
                    "[UNATTEND] DISM /Get-Intl 不可用，改用目标系统离线注册表只读回退: {:#}",
                    dism_error
                );
                match read_offline_international_settings_from_registry(image_path) {
                    Ok(settings) => {
                        log::info!(
                            "[UNATTEND] 已从目标系统离线注册表读取并验证国际化设置"
                        );
                        Ok(settings)
                    }
                    Err(registry_error) => bail!(
                        "无法读取目标系统国际化设置；DISM /Get-Intl 失败: {:#}; 离线注册表回退失败: {:#}",
                        dism_error,
                        registry_error
                    ),
                }
            }
        }
    }

    /// 解析 DISM 输出中的进度信息
    ///
    /// DISM 输出格式通常为:
    /// - "XX.X%"
    /// - "[==        ] XX.X%"
    fn parse_progress_line(line: &str) -> Option<DismExeProgress> {
        // 匹配百分比格式: "XX.X%" 或 "XX%"
        let trimmed = line.trim();

        // 检查是否包含百分比
        if let Some(percent_pos) = trimmed.find('%') {
            // 向前查找数字
            let before_percent = &trimmed[..percent_pos];
            let number_start = before_percent
                .rfind(|c: char| !c.is_ascii_digit() && c != '.')
                .map(|i| i + 1)
                .unwrap_or(0);

            if let Ok(percentage) = before_percent[number_start..].parse::<f32>() {
                let pct = (percentage as u8).min(100);
                return Some(DismExeProgress {
                    percentage: pct,
                    status: tr!("处理中 {}%", pct),
                });
            }
        }

        // 检查特定状态文本
        let lower = trimmed.to_lowercase();
        if lower.contains("完成") || lower.contains("successfully") || lower.contains("success") {
            return Some(DismExeProgress {
                percentage: 100,
                status: tr!("完成"),
            });
        }

        if lower.contains("正在") || lower.contains("processing") || lower.contains("adding") {
            return Some(DismExeProgress {
                percentage: 0,
                status: trimmed.to_string(),
            });
        }

        None
    }

    /// 从 DISM 输出中提取错误信息
    fn extract_error_from_output(output: &str) -> String {
        let lines: Vec<&str> = output.lines().collect();

        // 查找错误行
        for (i, line) in lines.iter().enumerate() {
            let lower = line.to_lowercase();
            if lower.contains("error") || lower.contains("错误") || lower.contains("失败") {
                // 返回错误行及后续几行作为上下文
                let end = (i + 3).min(lines.len());
                return lines[i..end].join("\n");
            }
        }

        // 返回最后几行作为错误信息
        let start = lines.len().saturating_sub(5);
        lines[start..].join("\n")
    }

    // =========================================================================
    // 公共 API - 驱动操作
    // =========================================================================

    /// 添加驱动到离线系统镜像
    ///
    /// 使用 dism.exe /Add-Driver 命令将驱动添加到离线 Windows 镜像。
    ///
    /// # 参数
    /// - `image_path`: 离线系统根目录（如 "D:\\"）
    /// - `driver_path`: 驱动目录或 INF 文件路径
    /// - `recurse`: 是否递归搜索子目录
    /// - `force_unsigned`: 是否强制安装未签名驱动
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 示例
    /// ```ignore
    /// let dism = DismExe::new()?;
    /// dism.add_driver_offline("D:\\", "C:\\Drivers", true, false, None)?;
    /// ```
    pub fn add_driver_offline(
        &self,
        image_path: &str,
        driver_path: &str,
        recurse: bool,
        force_unsigned: bool,
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<()> {
        // Never bypass catalog validation for an offline target. A rejected boot-start driver
        // otherwise becomes an unbootable installation instead of an actionable import error.
        if force_unsigned {
            bail!("refusing to add unsigned offline drivers");
        }
        log::info!(
            "[DISM.EXE] 添加驱动到离线系统: {} -> {}",
            driver_path,
            image_path
        );

        // 验证路径
        let driver_path_obj = Path::new(driver_path);
        if !driver_path_obj.exists() {
            bail!("{}", tr!("驱动路径不存在: {}", driver_path));
        }

        // 规范化镜像路径（确保以反斜杠结尾）
        let normalized_image = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };

        // 确保 scratchdir 存在
        let scratch_dir = Self::ensure_scratch_directory();

        // 构建命令参数
        let mut args = vec![
            "/Image:".to_string() + &normalized_image,
            "/Add-Driver".to_string(),
            "/Driver:".to_string() + driver_path,
        ];

        if recurse {
            args.push("/Recurse".to_string());
        }

        args.push(format!("/scratchdir:{}", scratch_dir));

        // 转换为 &str 切片
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        self.execute_with_progress(&args_ref, progress_tx)?;
        Ok(())
    }

    // =========================================================================
    // 公共 API - 更新包操作
    // =========================================================================

    /// 添加 Windows Update CAB 包到离线系统镜像
    ///
    /// 使用 dism.exe /Add-Package 命令安装 Windows Update 包。
    ///
    /// # 参数
    /// - `image_path`: 离线系统根目录（如 "D:\\"）
    /// - `package_path`: CAB 包文件路径
    /// - `ignore_check`: 是否忽略适用性检查
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 示例
    /// ```ignore
    /// let dism = DismExe::new()?;
    /// dism.add_package_offline("D:\\", "C:\\Updates\\KB12345.cab", false, None)?;
    /// ```
    pub fn add_package_offline(
        &self,
        image_path: &str,
        package_path: &str,
        ignore_check: bool,
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<()> {
        log::info!(
            "[DISM.EXE] 添加更新包到离线系统: {} -> {}",
            package_path,
            image_path
        );

        // 验证文件存在
        if !Path::new(package_path).exists() {
            bail!("{}", tr!("CAB 包文件不存在: {}", package_path));
        }

        // 规范化镜像路径
        let normalized_image = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };

        // 确保 scratchdir 存在
        let scratch_dir = Self::ensure_scratch_directory();

        // 构建命令参数
        let mut args = vec![
            "/Image:".to_string() + &normalized_image,
            "/Add-Package".to_string(),
            "/PackagePath:".to_string() + package_path,
            format!("/scratchdir:{}", scratch_dir),
        ];

        if ignore_check {
            args.push("/IgnoreCheck".to_string());
        }

        // 转换为 &str 切片
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        self.execute_with_progress(&args_ref, progress_tx)?;
        Ok(())
    }

    /// 批量添加 Windows Update CAB 包到离线系统镜像
    ///
    /// # 参数
    /// - `image_path`: 离线系统根目录
    /// - `package_paths`: CAB 包文件路径列表
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 返回
    /// - (成功数, 失败数)
    pub fn add_packages_batch(
        &self,
        image_path: &str,
        package_paths: &[PathBuf],
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<(usize, usize)> {
        let total = package_paths.len();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (index, package_path) in package_paths.iter().enumerate() {
            // 发送当前进度
            if let Some(ref tx) = progress_tx {
                let overall_pct = ((index * 100) / total.max(1)) as u8;
                let _ = tx.send(DismExeProgress {
                    percentage: overall_pct,
                    status: tr!("安装更新 {}/{}", index + 1, total),
                });
            }

            let package_str = package_path.to_string_lossy();
            match self.add_package_offline(image_path, &package_str, false, None) {
                Ok(_) => {
                    success_count += 1;
                    log::info!("[DISM.EXE] 更新包安装成功: {}", package_path.display());
                }
                Err(e) => {
                    fail_count += 1;
                    log::warn!(
                        "[DISM.EXE] 更新包安装失败: {} - {}",
                        package_path.display(),
                        e
                    );
                }
            }
        }

        // 发送完成进度
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismExeProgress {
                percentage: 100,
                status: tr!("完成: {} 成功, {} 失败", success_count, fail_count),
            });
        }

        log::info!(
            "[DISM.EXE] 批量更新包安装完成: 成功 {}, 失败 {}",
            success_count,
            fail_count
        );

        Ok((success_count, fail_count))
    }

    /// 搜索目录中的所有 CAB 文件并安装
    ///
    /// # 参数
    /// - `image_path`: 离线系统根目录
    /// - `cab_dir`: 包含 CAB 文件的目录
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 返回
    /// - (成功数, 失败数)
    pub fn add_packages_from_directory(
        &self,
        image_path: &str,
        cab_dir: &Path,
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<(usize, usize)> {
        // 收集所有 CAB 文件
        let cab_files = Self::find_cab_files(cab_dir);

        if cab_files.is_empty() {
            log::info!("[DISM.EXE] 目录中没有找到 CAB 文件: {}", cab_dir.display());
            return Ok((0, 0));
        }

        log::info!(
            "[DISM.EXE] 在 {} 中找到 {} 个 CAB 文件",
            cab_dir.display(),
            cab_files.len()
        );

        self.add_packages_batch(image_path, &cab_files, progress_tx)
    }

    /// 递归查找目录中的所有 CAB 文件
    fn find_cab_files(dir: &Path) -> Vec<PathBuf> {
        let mut cab_files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext.to_string_lossy().to_lowercase() == "cab" {
                            cab_files.push(path);
                        }
                    }
                } else if path.is_dir() {
                    // 递归搜索子目录
                    cab_files.extend(Self::find_cab_files(&path));
                }
            }
        }

        cab_files
    }
}

impl Default for DismExe {
    fn default() -> Self {
        Self::new().expect("无法创建 DismExe 实例")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_progress_line() {
        assert!(DismExe::parse_progress_line("50.0%").is_some());
        assert!(DismExe::parse_progress_line("[====      ] 40.0%").is_some());
        assert!(DismExe::parse_progress_line("操作成功完成").is_some());
        assert!(DismExe::parse_progress_line("The operation completed successfully.").is_some());
        assert!(DismExe::parse_progress_line("Random text").is_none());
    }

    #[test]
    fn test_extract_error() {
        let output = "Line 1\nError: Something went wrong\nDetails here\nMore info\nLast line";
        let error = DismExe::extract_error_from_output(output);
        assert!(error.contains("Error:"));
    }

    #[test]
    fn parses_offline_international_settings_for_chinese_image() {
        let output = r#"
Default system UI language : zh-CN
System locale : zh-CN
Default time zone : China Standard Time
Active keyboard(s) : 0804:00000804
Keyboard layered driver : Not installed.
"#;
        let settings = parse_offline_international_settings(output).unwrap();
        assert_eq!(settings.ui_language, "zh-CN");
        assert_eq!(settings.system_locale, "zh-CN");
        assert_eq!(settings.user_locale, "zh-CN");
        assert_eq!(settings.input_locale, "0804:00000804");
        assert_eq!(settings.time_zone, "China Standard Time");
    }

    #[test]
    fn rejects_incomplete_offline_international_settings() {
        let error = parse_offline_international_settings(
            "Default system UI language : en-US\nSystem locale : en-US\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("活动键盘布局"));
    }

    #[test]
    fn converts_registry_lcids_and_keyboard_layouts() {
        assert_eq!(locale_id_from_registry("0804").unwrap(), 0x0804);
        assert_eq!(locale_id_from_registry("0x0409").unwrap(), 0x0409);
        assert_eq!(
            input_locale_from_keyboard_layout("00000804").unwrap(),
            "0804:00000804"
        );
        assert_eq!(
            input_locale_from_keyboard_layout("d0010409").unwrap(),
            "0409:d0010409"
        );
    }

    #[cfg(windows)]
    #[test]
    fn converts_standard_windows_lcids_to_locale_names() {
        assert_eq!(locale_name_from_registry_id("0804").unwrap(), "zh-CN");
        assert_eq!(locale_name_from_registry_id("0409").unwrap(), "en-US");
    }

    #[test]
    fn rejects_invalid_registry_international_values() {
        assert!(locale_id_from_registry("not-a-lcid").is_err());
        assert!(input_locale_from_keyboard_layout("804").is_err());
        assert!(input_locale_from_keyboard_layout("0000080Z").is_err());
    }

    #[test]
    fn unsigned_driver_override_is_rejected_before_path_or_process_access() {
        let dism = DismExe::new().expect("DISM command boundary should initialize");
        let error = dism
            .add_driver_offline(r"Z:\missing-image", r"Z:\missing-driver", true, true, None)
            .expect_err("/ForceUnsigned must be rejected");
        assert!(error.to_string().contains("unsigned offline drivers"));
    }
}
