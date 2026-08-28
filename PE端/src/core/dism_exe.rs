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
use walkdir::WalkDir;

#[cfg(windows)]
use windows::Win32::Globalization::LCIDToLocaleName;
#[cfg(windows)]
use windows::{core::w, Win32::Storage::FileSystem::SearchPathW};

use crate::tr;
use crate::utils::encoding::gbk_to_utf8;

/// Windows CREATE_NO_WINDOW 标志，用于隐藏控制台窗口
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

static OFFLINE_INTL_HIVE_SEQUENCE: AtomicU32 = AtomicU32::new(1);

#[cfg(windows)]
fn search_path_for_dism() -> Result<Option<PathBuf>> {
    let mut buffer = vec![0u16; 260];
    loop {
        let length = unsafe {
            SearchPathW(
                None,
                w!("dism.exe"),
                None,
                Some(buffer.as_mut_slice()),
                None,
            )
        };
        if length == 0 {
            return Ok(None);
        }
        if length < buffer.len() as u32 {
            buffer.truncate(length as usize);
            let path = PathBuf::from(
                String::from_utf16(&buffer)
                    .context("SearchPathW 为 dism.exe 返回了无效的 UTF-16 路径")?,
            );
            return Ok(path.is_file().then_some(path));
        }
        // SearchPathW returns the required size including the terminating NUL.
        let required = length as usize;
        let next_size = if required <= buffer.len() {
            buffer.len().saturating_mul(2)
        } else {
            required
        };
        if next_size <= buffer.len() {
            bail!("SearchPathW 为 dism.exe 返回了无法扩展的路径长度");
        }
        buffer.resize(next_size, 0);
    }
}

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

fn validate_preserved_driver_inf_files(inf_files: &[PathBuf]) -> Result<()> {
    if inf_files.is_empty() {
        bail!("no authenticated INF files were supplied");
    }
    for inf in inf_files {
        if inf
            .extension()
            .and_then(|value| value.to_str())
            .is_none_or(|value| !value.eq_ignore_ascii_case("inf"))
            || !inf.is_file()
        {
            bail!(
                "authenticated driver path is not an INF file: {}",
                inf.display()
            );
        }
    }
    Ok(())
}

/// A process launch, pipe, read, or wait failure means DISM itself was unavailable. That is
/// different from DISM running and rejecting one optional package: retrying every INF cannot make
/// an unavailable servicing process trustworthy, and the caller must not downgrade it to a package
/// warning. `anyhow::Context` preserves the originating `std::io::Error` in the chain.
fn is_dism_infrastructure_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreservedDriverImportResult {
    /// Exact authenticated INF files that DISM accepted. When the initial batch succeeds this is
    /// the complete input set; after isolation it contains only successful individual packages.
    pub successful_inf_files: Vec<PathBuf>,
    /// Bounded-by-input diagnostics for exact INF files rejected by a running DISM process.
    pub failures: Vec<String>,
}

fn import_preserved_driver_infs_resilient<Batch, One>(
    inf_files: &[PathBuf],
    run_batch: Batch,
    mut run_one: One,
) -> Result<PreservedDriverImportResult>
where
    Batch: FnOnce() -> Result<()>,
    One: FnMut(&Path) -> Result<()>,
{
    validate_preserved_driver_inf_files(inf_files)?;
    match run_batch() {
        Ok(()) => {
            return Ok(PreservedDriverImportResult {
                successful_inf_files: inf_files.to_vec(),
                failures: Vec::new(),
            })
        }
        Err(error) if is_dism_infrastructure_error(&error) => {
            return Err(error).context("DISM infrastructure failed during driver batch import");
        }
        Err(error) => log::warn!(
            "[DISM.EXE] exact authenticated driver batch failed; isolating exact INF packages: {}",
            error
        ),
    }

    let mut result = PreservedDriverImportResult::default();
    for inf in inf_files {
        // The authenticated set was checked immediately before the batch. If an artifact vanished
        // in between, that is a structural/integrity failure, not an optional compatibility result.
        if !inf.is_file() {
            bail!(
                "authenticated driver INF disappeared during import: {}",
                inf.display()
            );
        }
        match run_one(inf) {
            Ok(()) => result.successful_inf_files.push(inf.clone()),
            Err(error) => {
                if is_dism_infrastructure_error(&error) {
                    return Err(error).with_context(|| {
                        format!(
                            "DISM infrastructure failed while importing {}",
                            inf.display()
                        )
                    });
                }
                // Whether a rejected package is optional is decided by the caller after comparing
                // the authenticated boot-path manifest with the target image's DISM inventory.
                // This layer only reports the exact package failure; labelling it optional here
                // used to hide the distinction between an unrelated printer/network package and a
                // real storage path.
                result.failures.push(format!(
                    "DISM rejected driver package {}: {error:#}",
                    inf.display()
                ));
            }
        }
    }
    Ok(result)
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
    /// 2. Win32 API 返回的当前系统目录
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

        // 使用 Win32 API 返回的实际系统目录；不得把正常系统或自定义 PE 的盘符写死为 C。
        if let Ok(system_directory) = lr_core::windows_compat::system_directory() {
            for path in [
                system_directory.join("dism.exe"),
                system_directory.join("Dism").join("dism.exe"),
            ] {
                if path.exists() {
                    return Ok(path);
                }
            }
        }

        // 最后使用 Win32 的进程搜索路径查找，避免启动 where.exe 并解析本地化输出。
        #[cfg(windows)]
        if let Some(path) = search_path_for_dism()? {
            return Ok(path);
        }

        bail!(
            "{}",
            tr!(
                "无法找到 dism.exe。请确保在 PE 环境或 Windows 系统中运行。\n\
             已搜索的路径:\n\
             - Windows API 返回的实际 System32\\dism.exe\n\
             - X:\\Windows\\System32\\dism.exe (常见 PE 环境)"
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
        const MAX_DISM_STDOUT_BYTES: usize = 32 * 1024 * 1024;
        const MAX_DISM_STDERR_BYTES: usize = 8 * 1024 * 1024;

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
        let stdout_handle = std::thread::spawn(move || -> Result<String> {
            let mut reader = BufReader::new(stdout);
            let mut output = String::new();
            let mut bytes = Vec::new();
            let mut total_bytes = 0_usize;

            loop {
                let read = reader
                    .read_until(b'\n', &mut bytes)
                    .context("读取 dism.exe stdout 失败")?;
                if read == 0 {
                    break;
                }
                total_bytes = total_bytes
                    .checked_add(read)
                    .ok_or_else(|| anyhow::anyhow!("dism.exe stdout 大小溢出"))?;
                if total_bytes > MAX_DISM_STDOUT_BYTES {
                    bail!(
                        "dism.exe stdout 超过 {} 字节上限，拒绝把截断输出当成完整结果",
                        MAX_DISM_STDOUT_BYTES
                    );
                }
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

            Ok(output)
        });

        // 读取 stderr
        let stderr_handle = std::thread::spawn(move || -> Result<String> {
            let mut reader = BufReader::new(stderr);
            let mut error_output = String::new();
            let mut bytes = Vec::new();
            let mut total_bytes = 0_usize;

            loop {
                let read = reader
                    .read_until(b'\n', &mut bytes)
                    .context("读取 dism.exe stderr 失败")?;
                if read == 0 {
                    break;
                }
                total_bytes = total_bytes
                    .checked_add(read)
                    .ok_or_else(|| anyhow::anyhow!("dism.exe stderr 大小溢出"))?;
                if total_bytes > MAX_DISM_STDERR_BYTES {
                    bail!(
                        "dism.exe stderr 超过 {} 字节上限，拒绝把截断输出当成完整结果",
                        MAX_DISM_STDERR_BYTES
                    );
                }
                while matches!(bytes.last(), Some(b'\r' | b'\n')) {
                    bytes.pop();
                }
                let decoded_line = gbk_to_utf8(&bytes);
                bytes.clear();

                error_output.push_str(&decoded_line);
                error_output.push('\n');

                log::trace!("[DISM.EXE STDERR] {}", decoded_line);
            }

            Ok(error_output)
        });

        // 等待进程完成
        let status_result = child.wait().context(tr!("等待 dism.exe 完成失败"));

        // A successful exit is not sufficient if either pipe was truncated or its reader panicked.
        // Inventory callers may otherwise mistake a valid-looking prefix for the complete target
        // Driver Store and produce a false boot-storage failure.
        let stdout_text = stdout_handle
            .join()
            .map_err(|_| anyhow::anyhow!("dism.exe stdout 读取线程异常终止"))??;
        let stderr_text = stderr_handle
            .join()
            .map_err(|_| anyhow::anyhow!("dism.exe stderr 读取线程异常终止"))??;
        let status = status_result?;

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

    fn normalized_offline_image_argument(image_path: &str) -> Result<String> {
        if image_path.trim().is_empty() {
            bail!("offline image path is empty");
        }
        let normalized = if image_path.ends_with('\\') {
            image_path.to_owned()
        } else {
            format!("{image_path}\\")
        };
        Ok(format!("/Image:{normalized}"))
    }

    fn build_get_driver_info_arguments(
        image_path: &str,
        package: &lr_core::dism_driver_inventory::OfflineDriverPackageDescriptor,
        scratch_dir: &str,
        log_path: &Path,
    ) -> Result<Vec<String>> {
        if scratch_dir.trim().is_empty() {
            bail!("DISM scratch directory is empty");
        }
        if !log_path.is_absolute() {
            bail!(
                "DISM inventory log path is not absolute: {}",
                log_path.display()
            );
        }
        let mut args = vec![
            "/English".to_owned(),
            Self::normalized_offline_image_argument(image_path)?,
            "/Get-DriverInfo".to_owned(),
            format!("/Driver:{}", package.published_name),
        ];
        args.extend([
            "/Format:List".to_owned(),
            format!("/ScratchDir:{scratch_dir}"),
            format!("/LogPath:{}", log_path.display()),
            "/LogLevel:2".to_owned(),
        ]);
        Ok(args)
    }

    /// Read-only command-line fallback for base WinPE images that include the supported DISM
    /// servicing executable but omit the optional `DismApi.dll` facade.
    ///
    /// `/English` makes Microsoft's documented report field names invariant. `/Get-Drivers /All`
    /// discovers both inbox and OEM published names, then one documented
    /// `/Get-DriverInfo /Driver:<published-name>` command queries each package's image-applicable
    /// models. Package failures remain bounded and isolated without first issuing a multi-`/Driver`
    /// command that affected older host servicing stacks reject with error 87.
    pub fn enumerate_offline_driver_candidates_command_line(
        &self,
        image_path: &str,
        log_path: &Path,
    ) -> Result<lr_core::dism_driver_inventory::OfflineDriverInventory> {
        const MAX_TOTAL_CANDIDATES: usize = 262_144;
        const MAX_RECORDED_FAILURES: usize = 32;
        const MAX_FAILURE_CHARS: usize = 1_024;

        let scratch_dir = Self::ensure_scratch_directory();
        let image_arg = Self::normalized_offline_image_argument(image_path)?;
        if !log_path.is_absolute() {
            bail!(
                "DISM inventory log path is not absolute: {}",
                log_path.display()
            );
        }
        let log_parent = log_path.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "DISM inventory log path has no parent: {}",
                log_path.display()
            )
        })?;
        if !log_parent.is_dir() {
            bail!(
                "DISM inventory log parent does not exist: {}",
                log_parent.display()
            );
        }
        let list_args = [
            "/English".to_owned(),
            image_arg,
            "/Get-Drivers".to_owned(),
            "/All".to_owned(),
            "/Format:List".to_owned(),
            format!("/ScratchDir:{scratch_dir}"),
            format!("/LogPath:{}", log_path.display()),
            "/LogLevel:2".to_owned(),
        ];
        let list_refs = list_args.iter().map(String::as_str).collect::<Vec<_>>();
        let package_output = self
            .execute_with_progress(&list_refs, None)
            .context("DISM command-line /Get-Drivers /All inventory failed")?;
        let packages =
            lr_core::dism_driver_inventory::parse_dism_get_drivers_english(&package_output)
                .context("parsing invariant-English DISM driver package inventory failed")?;

        let mut inventory = lr_core::dism_driver_inventory::OfflineDriverInventory::default();
        let record_failure =
            |inventory: &mut lr_core::dism_driver_inventory::OfflineDriverInventory,
             published_name: &str,
             error: &anyhow::Error| {
                let mut detail = format!("{error:#}");
                if detail.chars().count() > MAX_FAILURE_CHARS {
                    detail = detail.chars().take(MAX_FAILURE_CHARS - 1).collect();
                    detail.push('…');
                }
                if inventory.package_query_failures.len() < MAX_RECORDED_FAILURES {
                    inventory.package_query_failures.push(
                        lr_core::dism_driver_inventory::OfflineDriverPackageQueryFailure {
                            published_name: published_name.to_owned(),
                            // The command-line surface reports a process exit code rather than the
                            // API HRESULT. Keep an explicit diagnostic sentinel; coverage never
                            // depends on this value.
                            hresult: u32::MAX,
                            detail,
                        },
                    );
                } else {
                    inventory.omitted_package_query_failures =
                        inventory.omitted_package_query_failures.saturating_add(1);
                }
            };

        for package in packages {
            let package_result =
                (|| -> Result<Vec<lr_core::dism_driver_inventory::OfflineDriverCandidate>> {
                    let args = Self::build_get_driver_info_arguments(
                        image_path,
                        &package,
                        &scratch_dir,
                        log_path,
                    )?;
                    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                    let output = self.execute_with_progress(&refs, None)?;
                    lr_core::dism_driver_inventory::parse_dism_get_driver_info_english(
                        &output, &package,
                    )
                })();
            match package_result {
                Ok(candidates) => inventory.candidates.extend(candidates),
                Err(error) => record_failure(&mut inventory, &package.published_name, &error),
            }
            if inventory.candidates.len() > MAX_TOTAL_CANDIDATES {
                bail!(
                    "DISM command-line inventory exceeds {MAX_TOTAL_CANDIDATES} total driver models"
                );
            }
        }

        inventory.candidates.sort_by(|left, right| {
            left.published_name
                .to_ascii_lowercase()
                .cmp(&right.published_name.to_ascii_lowercase())
                .then_with(|| {
                    left.hardware_id
                        .to_ascii_lowercase()
                        .cmp(&right.hardware_id.to_ascii_lowercase())
                })
                .then_with(|| {
                    left.compatible_ids
                        .to_ascii_lowercase()
                        .cmp(&right.compatible_ids.to_ascii_lowercase())
                })
                .then_with(|| left.architecture.cmp(&right.architecture))
        });
        inventory.candidates.dedup_by(|left, right| {
            left.published_name
                .eq_ignore_ascii_case(&right.published_name)
                && left.hardware_id.eq_ignore_ascii_case(&right.hardware_id)
                && left
                    .compatible_ids
                    .eq_ignore_ascii_case(&right.compatible_ids)
                && left.architecture == right.architecture
        });
        Ok(inventory)
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
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 示例
    /// ```ignore
    /// let dism = DismExe::new()?;
    /// dism.add_driver_offline("D:\\", "C:\\Drivers", true, None)?;
    /// ```
    pub fn add_driver_offline(
        &self,
        image_path: &str,
        driver_path: &str,
        recurse: bool,
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<()> {
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

    /// Add only the manifest-authenticated INF paths supplied by the typed task. Microsoft DISM
    /// supports repeating `/Driver` on one `/Add-Driver` command and installs them in command-line
    /// order; this avoids both a slow process per package and a recursive scan that could discover
    /// a file outside the authenticated set.
    pub fn add_driver_inf_files_offline(
        &self,
        image_path: &str,
        inf_files: &[PathBuf],
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<()> {
        for inf in inf_files {
            if !inf.is_file() {
                bail!(
                    "authenticated driver path is not an INF file: {}",
                    inf.display()
                );
            }
        }
        let scratch_dir = Self::ensure_scratch_directory();
        let args = Self::build_add_driver_inf_arguments(image_path, inf_files, &scratch_dir)?;
        let args_ref = args.iter().map(String::as_str).collect::<Vec<_>>();
        self.execute_with_progress(&args_ref, progress_tx)?;
        Ok(())
    }

    fn build_add_driver_inf_arguments(
        image_path: &str,
        inf_files: &[PathBuf],
        scratch_dir: &str,
    ) -> Result<Vec<String>> {
        if image_path.trim().is_empty() {
            bail!("offline image path is empty");
        }
        if inf_files.is_empty() {
            bail!("no authenticated INF files were supplied");
        }
        if scratch_dir.trim().is_empty() {
            bail!("DISM scratch directory is empty");
        }
        let normalized_image = if image_path.ends_with('\\') {
            image_path.to_owned()
        } else {
            format!("{image_path}\\")
        };
        let mut args = vec![
            format!("/Image:{normalized_image}"),
            "/Add-Driver".to_owned(),
        ];
        for inf in inf_files {
            if inf
                .extension()
                .and_then(|value| value.to_str())
                .is_none_or(|value| !value.eq_ignore_ascii_case("inf"))
            {
                bail!(
                    "authenticated driver path is not an INF file: {}",
                    inf.display()
                );
            }
            args.push(format!("/Driver:{}", inf.display()));
        }
        args.push(format!("/scratchdir:{scratch_dir}"));
        Ok(args)
    }

    pub fn add_preserved_driver_inf_files_resilient(
        &self,
        image_path: &str,
        inf_files: &[PathBuf],
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<PreservedDriverImportResult> {
        import_preserved_driver_infs_resilient(
            inf_files,
            || self.add_driver_inf_files_offline(image_path, inf_files, progress_tx.clone()),
            |inf| {
                self.add_driver_offline(
                    image_path,
                    &inf.to_string_lossy(),
                    false,
                    progress_tx.clone(),
                )
            },
        )
    }

    /// Imports a directory with standard DISM policy first, then isolates failures to exact INF
    /// packages without adding a second signing-policy implementation.
    pub fn add_drivers_from_directory_resilient(
        &self,
        image_path: &str,
        driver_dir: &str,
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<()> {
        let failures =
            self.add_drivers_from_directory_impl(image_path, driver_dir, progress_tx, false)?;
        debug_assert!(failures.is_empty());
        Ok(())
    }

    fn add_drivers_from_directory_impl(
        &self,
        image_path: &str,
        driver_dir: &str,
        progress_tx: Option<Sender<DismExeProgress>>,
        tolerate_package_failures: bool,
    ) -> Result<Vec<String>> {
        match self.add_driver_offline(image_path, driver_dir, true, progress_tx.clone()) {
            Ok(()) => return Ok(Vec::new()),
            Err(batch_error) => log::warn!(
                "[DISM.EXE] recursive import failed; retrying exact INF packages: {}",
                batch_error
            ),
        }

        let mut inf_files = Vec::new();
        for entry in WalkDir::new(driver_dir).follow_links(false) {
            let entry = entry
                .with_context(|| format!("failed to enumerate driver directory: {}", driver_dir))?;
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.eq_ignore_ascii_case("inf"))
                    == Some(true)
            {
                inf_files.push(entry.path().to_path_buf());
            }
        }
        inf_files.sort();
        if inf_files.is_empty() {
            bail!("driver directory contains no INF files: {}", driver_dir);
        }

        let mut failures = Vec::new();
        for inf in inf_files {
            let inf_text = inf.to_string_lossy();
            if let Err(normal_error) =
                self.add_driver_offline(image_path, &inf_text, false, progress_tx.clone())
            {
                let failure = format!(
                    "DISM rejected driver package {}: {normal_error:#}",
                    inf.display()
                );
                if tolerate_package_failures {
                    failures.push(failure);
                    continue;
                }
                bail!("{failure}");
            }
        }
        Ok(failures)
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

        self.add_packages_offline_ordered(
            image_path,
            &[PathBuf::from(package_path)],
            ignore_check,
            progress_tx,
        )
    }

    /// 在同一个 DISM servicing 会话中按给定顺序添加多个离线包。
    ///
    /// 依赖包必须由调用方按依赖顺序传入。DISM 官方支持重复的
    /// `/PackagePath` 参数，并按命令行顺序处理；这对 Windows 7 NVMe
    /// 热修补包这种有关联关系的组合尤为重要。
    pub fn add_packages_offline_ordered(
        &self,
        image_path: &str,
        package_paths: &[PathBuf],
        ignore_check: bool,
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<()> {
        for package_path in package_paths {
            if !package_path.is_file() {
                bail!("{}", tr!("CAB 包文件不存在: {}", package_path.display()));
            }
        }
        let scratch_dir = Self::ensure_scratch_directory();
        let args = Self::build_add_packages_offline_arguments(
            image_path,
            package_paths,
            ignore_check,
            &scratch_dir,
        )?;
        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
        self.execute_with_progress(&args_ref, progress_tx)?;
        Ok(())
    }

    fn build_add_packages_offline_arguments(
        image_path: &str,
        package_paths: &[PathBuf],
        ignore_check: bool,
        scratch_dir: &str,
    ) -> Result<Vec<String>> {
        if image_path.trim().is_empty() {
            bail!("offline image path is empty");
        }
        if package_paths.is_empty() {
            bail!("no offline packages were supplied");
        }
        if scratch_dir.trim().is_empty() {
            bail!("DISM scratch directory is empty");
        }

        let normalized_image = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };
        let mut args = vec![
            format!("/Image:{normalized_image}"),
            "/Add-Package".to_string(),
        ];
        for package_path in package_paths {
            args.push(format!("/PackagePath:{}", package_path.display()));
        }
        args.push(format!("/scratchdir:{scratch_dir}"));
        if ignore_check {
            args.push("/IgnoreCheck".to_string());
        }
        Ok(args)
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
        let mut failed_packages = lr_core::bounded_failure_summary::BoundedFailureCollector::new(3);

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
                }
                Err(e) => {
                    failed_packages.push(format_args!("{}: {e}", package_path.display()));
                }
            }
        }

        let failure_summary = failed_packages.finish();

        // 发送完成进度
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismExeProgress {
                percentage: 100,
                status: tr!(
                    "完成: {} 成功, {} 失败",
                    success_count,
                    failure_summary.total()
                ),
            });
        }

        log::info!(
            "[DISM.EXE] 批量更新包安装完成: 成功 {}, 失败 {}",
            success_count,
            failure_summary.total()
        );

        if !failure_summary.is_empty() {
            log::warn!("[DISM.EXE] 部分可选 CAB 未能安装: {failure_summary}");
        }
        Ok((success_count, failure_summary.total()))
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
    use std::cell::Cell;

    fn driver_test_tree(
        names: &[&str],
    ) -> (lr_core::scoped_temp_file::ScopedTempDir, Vec<PathBuf>) {
        let temporary = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-pe-driver-import",
        )
        .expect("temporary driver directory");
        let mut files = Vec::new();
        for name in names {
            let path = temporary.path().join(name);
            std::fs::write(&path, b"[Version]\r\nSignature=\"$Windows NT$\"\r\n")
                .expect("write test INF");
            files.push(path);
        }
        (temporary, files)
    }

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
    fn ordered_package_arguments_preserve_dependency_order_in_one_transaction() {
        let packages = vec![
            PathBuf::from(r"R:\nvme\Windows6.1-KB2990941-v3-x64.cab"),
            PathBuf::from(r"R:\nvme\Windows6.1-KB3087873-v2-x64.cab"),
        ];
        let args = DismExe::build_add_packages_offline_arguments(
            r"C:",
            &packages,
            false,
            r"X:\Windows\Temp",
        )
        .unwrap();
        assert_eq!(args[0], r"/Image:C:\");
        assert_eq!(args[1], "/Add-Package");
        assert_eq!(
            args[2],
            r"/PackagePath:R:\nvme\Windows6.1-KB2990941-v3-x64.cab"
        );
        assert_eq!(
            args[3],
            r"/PackagePath:R:\nvme\Windows6.1-KB3087873-v2-x64.cab"
        );
        assert_eq!(args[4], r"/scratchdir:X:\Windows\Temp");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn ordered_package_arguments_reject_empty_inputs() {
        assert!(DismExe::build_add_packages_offline_arguments(
            r"C:\",
            &[],
            false,
            r"X:\Windows\Temp"
        )
        .is_err());
        assert!(DismExe::build_add_packages_offline_arguments(
            "",
            &[PathBuf::from(r"R:\nvme\one.cab")],
            false,
            r"X:\Windows\Temp"
        )
        .is_err());
    }

    #[test]
    fn exact_driver_arguments_repeat_driver_without_recursive_discovery() {
        let args = DismExe::build_add_driver_inf_arguments(
            r"C:\",
            &[
                PathBuf::from(r"R:\drivers\wifi\net.inf"),
                PathBuf::from(r"R:\drivers\storage\stor.INF"),
            ],
            r"X:\Windows\Temp",
        )
        .unwrap();
        assert_eq!(args[0], r"/Image:C:\");
        assert_eq!(args[1], "/Add-Driver");
        assert_eq!(args[2], r"/Driver:R:\drivers\wifi\net.inf");
        assert_eq!(args[3], r"/Driver:R:\drivers\storage\stor.INF");
        assert_eq!(args[4], r"/scratchdir:X:\Windows\Temp");
        assert!(!args.iter().any(|arg| arg.eq_ignore_ascii_case("/Recurse")));
    }

    #[test]
    fn exact_driver_arguments_reject_empty_or_non_inf_inputs() {
        assert!(DismExe::build_add_driver_inf_arguments(r"C:\", &[], r"X:\Windows\Temp").is_err());
        assert!(DismExe::build_add_driver_inf_arguments(
            r"C:\",
            &[PathBuf::from(r"R:\drivers\payload.sys")],
            r"X:\Windows\Temp"
        )
        .is_err());
    }

    #[test]
    fn target_inventory_detail_arguments_use_one_invariant_read_only_published_name() {
        let package = lr_core::dism_driver_inventory::OfflineDriverPackageDescriptor {
            published_name: "storvsc.inf".into(),
            original_file_name: "storvsc.inf".into(),
            class_name: "SCSIAdapter".into(),
            in_box: true,
        };
        let args = DismExe::build_get_driver_info_arguments(
            r"D:\",
            &package,
            r"X:\Windows\Temp",
            Path::new(r"X:\Windows\Temp\driver-inventory.log"),
        )
        .unwrap();
        assert_eq!(args[0], "/English");
        assert_eq!(args[1], r"/Image:D:\");
        assert_eq!(args[2], "/Get-DriverInfo");
        assert_eq!(args[3], "/Driver:storvsc.inf");
        assert_eq!(
            args.iter()
                .filter(|arg| arg.starts_with("/Driver:"))
                .count(),
            1
        );
        assert!(args.iter().any(|arg| arg == "/Format:List"));
        assert!(!args.iter().any(|arg| {
            arg.eq_ignore_ascii_case("/Add-Driver")
                || arg.eq_ignore_ascii_case("/ForceUnsigned")
                || arg.eq_ignore_ascii_case("/Recurse")
        }));
    }

    #[test]
    fn resilient_driver_import_stops_on_empty_or_structurally_invalid_authenticated_sets() {
        assert!(import_preserved_driver_infs_resilient(&[], || Ok(()), |_| Ok(())).is_err());

        let (temporary, mut files) = driver_test_tree(&["valid.inf"]);
        files.push(temporary.path().join("missing.inf"));
        let batch_ran = Cell::new(false);
        assert!(import_preserved_driver_infs_resilient(
            &files,
            || {
                batch_ran.set(true);
                Ok(())
            },
            |_| Ok(())
        )
        .is_err());
        assert!(!batch_ran.get());
    }

    #[test]
    fn resilient_driver_import_batch_success_never_runs_individual_fallback() {
        let (_temporary, files) = driver_test_tree(&["printer.inf", "network.inf"]);
        let batch_runs = Cell::new(0_u32);
        let individual_runs = Cell::new(0_u32);
        let result = import_preserved_driver_infs_resilient(
            &files,
            || {
                batch_runs.set(batch_runs.get() + 1);
                Ok(())
            },
            |_| {
                individual_runs.set(individual_runs.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert!(result.failures.is_empty());
        assert_eq!(result.successful_inf_files, files);
        assert_eq!(batch_runs.get(), 1);
        assert_eq!(individual_runs.get(), 0);
    }

    #[test]
    fn resilient_driver_import_isolates_single_and_all_optional_package_failures() {
        let (_temporary, files) =
            driver_test_tree(&["printer.inf", "bad-network.inf", "virtual-device.inf"]);
        let attempts = Cell::new(0_u32);
        let result = import_preserved_driver_infs_resilient(
            &files,
            || anyhow::bail!("DISM batch rejected one package"),
            |inf| {
                attempts.set(attempts.get() + 1);
                if inf
                    .file_name()
                    .is_some_and(|name| name == "bad-network.inf")
                {
                    anyhow::bail!("package is not applicable");
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(attempts.get(), 3);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].contains("bad-network.inf"));
        assert_eq!(result.successful_inf_files.len(), 2);

        let all_failures = import_preserved_driver_infs_resilient(
            &files,
            || anyhow::bail!("DISM batch rejected all packages"),
            |_| anyhow::bail!("package is not applicable"),
        )
        .unwrap();
        assert_eq!(all_failures.failures.len(), files.len());
        assert!(all_failures.successful_inf_files.is_empty());
    }

    #[test]
    fn resilient_driver_import_never_downgrades_dism_infrastructure_failure() {
        let (_temporary, files) = driver_test_tree(&["printer.inf", "network.inf"]);
        let individual_runs = Cell::new(0_u32);
        let batch_error = import_preserved_driver_infs_resilient(
            &files,
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "dism.exe could not be started",
                )
                .into())
            },
            |_| {
                individual_runs.set(individual_runs.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(batch_error.to_string().contains("infrastructure"));
        assert_eq!(individual_runs.get(), 0);

        let individual_error = import_preserved_driver_infs_resilient(
            &files,
            || anyhow::bail!("DISM batch rejected one package"),
            |_| {
                Err(
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "lost DISM output pipe")
                        .into(),
                )
            },
        )
        .unwrap_err();
        assert!(individual_error.to_string().contains("infrastructure"));
    }

    #[test]
    fn resilient_driver_import_treats_authenticated_artifact_disappearance_as_fatal() {
        let (_temporary, files) = driver_test_tree(&["first.inf", "second.inf"]);
        let second = files[1].clone();
        let error = import_preserved_driver_infs_resilient(
            &files,
            || anyhow::bail!("DISM batch rejected one package"),
            |inf| {
                if inf.file_name().is_some_and(|name| name == "first.inf") {
                    std::fs::remove_file(&second).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("disappeared"));
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
}
