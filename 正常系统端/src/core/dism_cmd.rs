//! DISM 命令行封装模块
//!
//! 提供基于 dism.exe 命令行的 Windows 镜像服务功能：
//! - 离线驱动导入（Add-Driver）
//! - 离线 CAB 包导入（Add-Package）
//! - 驱动导出
//!
//! 优先使用程序目录下的 `bin\Dism\dism.exe`，
//! 如果不存在则回退到系统 DISM。
//!
//! 注意：该模块可在正常系统和 PE 环境下运行，
//! 临时目录会根据运行环境自动选择。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{bail, Context, Result};

use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_exe_dir;

/// DISM 操作进度
#[derive(Debug, Clone)]
pub struct DismCmdProgress {
    /// 进度百分比 (0-100)
    pub percentage: u8,
    /// 状态描述
    pub status: String,
}

/// DISM 命令行执行器
///
/// 封装 dism.exe 的命令行调用，提供：
/// - 自动查找最优 DISM 路径
/// - 隐藏控制台窗口
/// - 实时进度解析
/// - 完善的错误处理
/// - 智能临时目录选择（支持 PE 和正常系统环境）
pub struct DismCmd {
    dism_path: PathBuf,
}

impl DismCmd {
    /// 创建 DISM 命令行执行器
    ///
    /// 按以下优先级查找 dism.exe：
    /// 1. `{程序目录}\bin\Dism\dism.exe`
    /// 2. PE 环境路径 (X:\Windows\System32\dism.exe 等)
    /// 3. 系统 DISM
    pub fn new() -> Result<Self> {
        let dism_path = Self::find_dism_executable()?;
        log::info!("[DismCmd] 使用 DISM: {}", dism_path.display());
        Ok(Self { dism_path })
    }

    /// 查找 DISM 可执行文件
    fn find_dism_executable() -> Result<PathBuf> {
        // 优先级1: 程序目录下的 bin\Dism\dism.exe
        let local_dism = get_exe_dir().join("bin").join("Dism").join("dism.exe");
        if local_dism.exists() {
            log::info!("[DismCmd] 找到本地 DISM: {}", local_dism.display());
            return Ok(local_dism);
        }

        // 优先级2: PE 环境路径
        let pe_paths = [
            PathBuf::from(r"X:\Windows\System32\dism.exe"),
            PathBuf::from(r"X:\Windows\System32\Dism\dism.exe"),
        ];
        for path in &pe_paths {
            if path.exists() {
                log::info!("[DismCmd] 找到 PE 环境 DISM: {}", path.display());
                return Ok(path.clone());
            }
        }

        // 优先级3: 尝试检测 PE 环境的其他可能盘符
        for letter in ['X', 'Y', 'Z', 'W'] {
            let path = PathBuf::from(format!(r"{}:\Windows\System32\dism.exe", letter));
            if path.exists() {
                log::info!("[DismCmd] 找到 PE 环境 DISM: {}", path.display());
                return Ok(path);
            }
        }

        // 优先级4: 检查系统 DISM 是否可用
        let system_dism = PathBuf::from("dism.exe");
        if Self::verify_dism_available(&system_dism) {
            log::info!("[DismCmd] 使用系统 DISM");
            return Ok(system_dism);
        }

        // 优先级5: 尝试 System32 目录
        if let Ok(windir) = std::env::var("WINDIR") {
            let system32_dism = PathBuf::from(&windir).join("System32").join("Dism.exe");
            if system32_dism.exists() {
                log::info!("[DismCmd] 找到 System32 DISM: {}", system32_dism.display());
                return Ok(system32_dism);
            }
        }

        // 优先级6: 常见系统路径
        let system_paths = [
            PathBuf::from(r"C:\Windows\System32\dism.exe"),
            PathBuf::from(r"C:\Windows\System32\Dism\dism.exe"),
        ];
        for path in &system_paths {
            if path.exists() {
                log::info!("[DismCmd] 找到系统 DISM: {}", path.display());
                return Ok(path.clone());
            }
        }

        bail!(
            "未找到可用的 dism.exe。请确保系统已安装 DISM 或将 dism.exe 放置于程序目录的 bin\\Dism\\ 下\n\
             已搜索路径:\n\
             - {{程序目录}}\\bin\\Dism\\dism.exe\n\
             - X:\\Windows\\System32\\dism.exe (PE 环境)\n\
             - C:\\Windows\\System32\\dism.exe (Windows 系统)"
        )
    }

    /// 验证 DISM 是否可用
    fn verify_dism_available(dism_path: &Path) -> bool {
        new_command(dism_path)
            .arg("/?")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// 获取 DISM 路径
    pub fn dism_path(&self) -> &Path {
        &self.dism_path
    }

    /// 确保临时目录存在并返回路径
    ///
    /// 智能选择临时目录，按优先级：
    /// 1. PE 环境临时目录 (X:\Windows\TEMP, X:\TEMP 等)
    /// 2. 程序运行目录下的 temp 目录
    /// 3. 系统临时目录
    ///
    /// 该方法会确保返回的目录存在且可写。
    fn ensure_scratch_directory() -> String {
        // PE 环境候选目录（按优先级排序）
        let pe_candidates = [
            r"X:\Windows\TEMP",
            r"X:\TEMP",
            r"Y:\Windows\TEMP",
            r"Y:\TEMP",
            r"Z:\Windows\TEMP",
            r"Z:\TEMP",
        ];

        // 首先检查 PE 环境目录
        for dir in &pe_candidates {
            let path = Path::new(dir);
            if path.exists() {
                log::debug!("[DismCmd] 使用 PE 临时目录: {}", dir);
                return dir.to_string();
            }

            // 尝试创建目录（可能盘符存在但目录不存在）
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    if std::fs::create_dir_all(path).is_ok() {
                        log::info!("[DismCmd] 创建 PE 临时目录: {}", dir);
                        return dir.to_string();
                    }
                }
            }
        }

        // 尝试使用程序运行目录下的 temp 目录
        let exe_temp = get_exe_dir().join("temp");
        if std::fs::create_dir_all(&exe_temp).is_ok() {
            let temp_str = exe_temp.to_string_lossy().to_string();
            log::info!("[DismCmd] 使用程序临时目录: {}", temp_str);
            return temp_str;
        }

        // 最后回退到系统临时目录
        let system_temp = std::env::temp_dir();
        let temp_str = system_temp.to_string_lossy().to_string();
        log::info!("[DismCmd] 使用系统临时目录: {}", temp_str);

        // 确保系统临时目录存在
        let _ = std::fs::create_dir_all(&system_temp);
        temp_str
    }

    // ========================================================================
    // 离线驱动操作
    // ========================================================================

    /// 向离线映像添加驱动
    ///
    /// 等效于: `dism /Image:<image_path> /Add-Driver /Driver:<driver_path> /Recurse /scratchdir:<temp>`
    ///
    /// # 参数
    /// - `image_path`: 离线映像路径（挂载点或 Windows 根目录，如 `D:\`）
    /// - `driver_path`: 驱动路径（INF 文件或包含驱动的目录）
    /// - `recurse`: 是否递归搜索子目录
    /// - `force_unsigned`: 是否强制安装未签名驱动
    /// - `progress_tx`: 可选的进度发送器
    ///
    /// # 返回
    /// - `Ok(())` 表示成功
    /// - `Err` 包含详细错误信息
    pub fn add_driver_offline(
        &self,
        image_path: &str,
        driver_path: &str,
        recurse: bool,
        force_unsigned: bool,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        // 规范化路径（确保以反斜杠结尾，与 PE 端保持一致）
        let image_path = Self::normalize_image_path(image_path);
        let driver_path_normalized = driver_path.trim().to_string();

        // 验证路径
        if !Path::new(&image_path.trim_end_matches('\\')).exists() {
            bail!("离线映像路径不存在: {}", image_path);
        }
        if !Path::new(&driver_path_normalized).exists() {
            bail!("驱动路径不存在: {}", driver_path_normalized);
        }

        log::info!(
            "[DismCmd] 添加驱动: {} -> {}",
            driver_path_normalized,
            image_path
        );

        // 发送初始进度
        Self::send_progress(&progress_tx, 0, "正在准备添加驱动...");

        // 确保临时目录存在
        let scratch_dir = Self::ensure_scratch_directory();

        // 构建命令参数
        let mut args = vec![
            format!("/Image:{}", image_path),
            "/Add-Driver".to_string(),
            format!("/Driver:{}", driver_path_normalized),
            format!("/scratchdir:{}", scratch_dir),
        ];

        if recurse {
            args.push("/Recurse".to_string());
        }

        if force_unsigned {
            args.push("/ForceUnsigned".to_string());
        }

        // 执行命令
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.execute_with_progress_args(&args_ref, progress_tx, "驱动添加")
    }

    /// 批量添加驱动目录
    ///
    /// 递归扫描目录中的所有驱动并添加到离线映像
    ///
    /// # 参数
    /// - `image_path`: 离线映像路径
    /// - `driver_dir`: 驱动目录
    /// - `progress_tx`: 可选的进度发送器
    pub fn add_drivers_from_directory(
        &self,
        image_path: &str,
        driver_dir: &str,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        // 直接使用 /Recurse 参数一次性添加整个目录
        self.add_driver_offline(image_path, driver_dir, true, true, progress_tx)
    }

    // ========================================================================
    // CAB 包操作
    // ========================================================================

    /// 向离线映像添加 CAB 包（Windows 更新包）
    ///
    /// 等效于: `dism /Image:<image_path> /Add-Package /PackagePath:<cab_path> /scratchdir:<temp>`
    ///
    /// # 参数
    /// - `image_path`: 离线映像路径
    /// - `package_path`: CAB 包路径
    /// - `ignore_check`: 是否忽略适用性检查
    /// - `progress_tx`: 可选的进度发送器
    pub fn add_package_offline(
        &self,
        image_path: &str,
        package_path: &str,
        ignore_check: bool,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        // 规范化路径
        let image_path = Self::normalize_image_path(image_path);
        let package_path = package_path.trim().to_string();

        if !Path::new(&image_path.trim_end_matches('\\')).exists() {
            bail!("离线映像路径不存在: {}", image_path);
        }
        if !Path::new(&package_path).exists() {
            bail!("包路径不存在: {}", package_path);
        }

        log::info!("[DismCmd] 添加包: {} -> {}", package_path, image_path);

        Self::send_progress(&progress_tx, 0, "正在准备添加更新包...");

        // 确保临时目录存在
        let scratch_dir = Self::ensure_scratch_directory();

        // 构建命令参数
        let mut args = vec![
            format!("/Image:{}", image_path),
            "/Add-Package".to_string(),
            format!("/PackagePath:{}", package_path),
            format!("/scratchdir:{}", scratch_dir),
        ];

        if ignore_check {
            args.push("/IgnoreCheck".to_string());
        }

        // 执行命令
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.execute_with_progress_args(&args_ref, progress_tx, "包添加")
    }

    /// 向离线映像添加 CAB 包（简化版，兼容旧接口）
    pub fn add_package_offline_simple(
        &self,
        image_path: &str,
        package_path: &str,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        self.add_package_offline(image_path, package_path, false, progress_tx)
    }

    /// 批量添加 CAB 包
    ///
    /// 扫描目录中的所有 .cab 文件并添加到离线映像
    ///
    /// # 参数
    /// - `image_path`: 离线映像路径
    /// - `package_dir`: 包含 CAB 文件的目录
    /// - `progress_tx`: 可选的进度发送器
    pub fn add_packages_from_directory(
        &self,
        image_path: &str,
        package_dir: &str,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        let package_dir_path = Path::new(package_dir);
        if !package_dir_path.exists() {
            bail!("包目录不存在: {}", package_dir);
        }

        // 收集所有 CAB 文件
        let cab_files: Vec<PathBuf> = Self::find_cab_files(package_dir_path)?;

        if cab_files.is_empty() {
            log::info!("[DismCmd] 目录中没有 CAB 文件: {}", package_dir);
            return Ok(());
        }

        log::info!("[DismCmd] 找到 {} 个 CAB 文件", cab_files.len());

        let total = cab_files.len();
        let mut success_count = 0;
        let mut failed_packages = Vec::new();

        for (idx, cab_path) in cab_files.iter().enumerate() {
            let progress_pct = ((idx * 100) / total) as u8;
            let cab_name = cab_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown.cab");

            Self::send_progress(
                &progress_tx,
                progress_pct,
                &format!("正在添加: {} ({}/{})", cab_name, idx + 1, total),
            );

            match self.add_package_offline(
                image_path,
                &cab_path.to_string_lossy(),
                false,
                None, // 内部不再发送进度
            ) {
                Ok(_) => {
                    success_count += 1;
                    log::info!("[DismCmd] 成功添加: {}", cab_name);
                }
                Err(e) => {
                    log::warn!("[DismCmd] 添加失败: {} - {}", cab_name, e);
                    failed_packages.push(cab_name.to_string());
                }
            }
        }

        Self::send_progress(&progress_tx, 100, "CAB 包添加完成");

        log::info!(
            "[DismCmd] CAB 包添加完成: 成功 {}/{}, 失败 {}",
            success_count,
            total,
            failed_packages.len()
        );

        if success_count == 0 && !cab_files.is_empty() {
            bail!("所有 CAB 包添加失败: {:?}", failed_packages);
        }

        Ok(())
    }

    // ========================================================================
    // 驱动导出
    // ========================================================================

    /// 从离线映像导出驱动
    ///
    /// 等效于: `dism /Image:<image_path> /Export-Driver /Destination:<dest_path>`
    ///
    /// # 参数
    /// - `image_path`: 离线映像路径
    /// - `destination`: 导出目标目录
    /// - `progress_tx`: 可选的进度发送器
    pub fn export_drivers_offline(
        &self,
        image_path: &str,
        destination: &str,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        let image_path = Self::normalize_image_path(image_path);
        let destination = destination.trim().to_string();

        if !Path::new(&image_path.trim_end_matches('\\')).exists() {
            bail!("离线映像路径不存在: {}", image_path);
        }

        // 确保目标目录存在
        std::fs::create_dir_all(&destination).context("创建导出目录失败")?;

        log::info!("[DismCmd] 导出驱动: {} -> {}", image_path, destination);

        Self::send_progress(&progress_tx, 0, "正在准备导出驱动...");

        // 确保临时目录存在
        let scratch_dir = Self::ensure_scratch_directory();

        let args = [
            &format!("/Image:{}", image_path),
            "/Export-Driver",
            &format!("/Destination:{}", destination),
            &format!("/scratchdir:{}", scratch_dir),
        ];

        self.execute_with_progress_args(&args, progress_tx, "驱动导出")
    }

    // ========================================================================
    // 综合驱动和 CAB 导入
    // ========================================================================

    /// 智能导入驱动目录（支持普通驱动和 CAB 包混合）
    ///
    /// 此函数会智能识别目录内容：
    /// - 普通驱动文件（.inf）使用 /Add-Driver
    /// - CAB 包文件（.cab）使用 /Add-Package
    ///
    /// # 参数
    /// - `image_path`: 离线映像路径
    /// - `source_dir`: 源目录（可包含驱动和 CAB 包）
    /// - `progress_tx`: 可选的进度发送器
    pub fn import_drivers_smart(
        &self,
        image_path: &str,
        source_dir: &str,
        progress_tx: Option<Sender<DismCmdProgress>>,
    ) -> Result<()> {
        let source_path = Path::new(source_dir);
        if !source_path.exists() {
            bail!("源目录不存在: {}", source_dir);
        }

        // 分析目录内容
        let (has_inf_files, has_cab_files) = Self::analyze_directory(source_path);

        log::info!(
            "[DismCmd] 目录分析: INF={}, CAB={}",
            has_inf_files,
            has_cab_files
        );

        let mut last_error: Option<anyhow::Error> = None;

        // 处理 CAB 包（Windows 更新）
        if has_cab_files {
            Self::send_progress(&progress_tx, 0, "正在添加 CAB 更新包...");

            if let Err(e) = self.add_packages_from_directory(image_path, source_dir, None) {
                log::warn!("[DismCmd] CAB 包添加失败: {}", e);
                last_error = Some(e);
            }
        }

        // 处理普通驱动
        if has_inf_files {
            Self::send_progress(
                &progress_tx,
                if has_cab_files { 50 } else { 0 },
                "正在添加驱动...",
            );

            if let Err(e) = self.add_drivers_from_directory(image_path, source_dir, None) {
                log::warn!("[DismCmd] 驱动添加失败: {}", e);
                // 如果 CAB 处理成功但驱动失败，记录错误但不立即返回
                if last_error.is_none() {
                    last_error = Some(e);
                }
            }
        }

        Self::send_progress(&progress_tx, 100, "导入完成");

        // 如果两个操作都失败了，返回错误
        if !has_inf_files && !has_cab_files {
            bail!("目录中没有找到驱动文件（.inf）或 CAB 包（.cab）");
        }

        if let Some(e) = last_error {
            // 如果有部分成功，只打印警告
            if has_inf_files && has_cab_files {
                log::warn!("[DismCmd] 部分导入失败: {}", e);
                return Ok(());
            }
            return Err(e);
        }

        Ok(())
    }

    // ========================================================================
    // 信息查询
    // ========================================================================

    /// 获取离线系统中已安装的驱动列表
    pub fn get_drivers(&self, image_path: &str) -> Result<String> {
        let image_path = Self::normalize_image_path(image_path);
        let scratch_dir = Self::ensure_scratch_directory();

        let args = [
            &format!("/Image:{}", image_path),
            "/Get-Drivers",
            &format!("/scratchdir:{}", scratch_dir),
        ];

        self.execute_and_get_output(&args)
    }

    /// 获取离线系统中已安装的更新包列表
    pub fn get_packages(&self, image_path: &str) -> Result<String> {
        let image_path = Self::normalize_image_path(image_path);
        let scratch_dir = Self::ensure_scratch_directory();

        let args = [
            &format!("/Image:{}", image_path),
            "/Get-Packages",
            &format!("/scratchdir:{}", scratch_dir),
        ];

        self.execute_and_get_output(&args)
    }

    // ========================================================================
    // 内部辅助方法
    // ========================================================================

    /// 规范化镜像路径（确保以反斜杠结尾）
    fn normalize_image_path(path: &str) -> String {
        let path = path.trim();
        if path.ends_with('\\') {
            path.to_string()
        } else {
            format!("{}\\", path)
        }
    }

    /// 执行命令并获取输出
    fn execute_and_get_output(&self, args: &[&str]) -> Result<String> {
        log::info!(
            "[DismCmd] 执行: {} {}",
            self.dism_path.display(),
            args.join(" ")
        );

        let mut cmd = new_command(&self.dism_path);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().context("执行 DISM 命令失败")?;

        let stdout = if output.stdout.is_empty() {
            String::new()
        } else {
            // 尝试转换编码
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            if stdout_str.contains('\u{FFFD}') {
                gbk_to_utf8(&output.stdout)
            } else {
                stdout_str.to_string()
            }
        };

        if !output.status.success() {
            let stderr = if output.stderr.is_empty() {
                String::new()
            } else {
                let stderr_str = String::from_utf8_lossy(&output.stderr);
                if stderr_str.contains('\u{FFFD}') {
                    gbk_to_utf8(&output.stderr)
                } else {
                    stderr_str.to_string()
                }
            };

            let error_msg = if !stderr.trim().is_empty() {
                stderr
            } else if !stdout.trim().is_empty() {
                Self::extract_error_from_output(&stdout)
            } else {
                format!("DISM 退出码: {:?}", output.status.code())
            };

            bail!("DISM 操作失败: {}", error_msg);
        }

        Ok(stdout)
    }

    /// 使用参数执行命令并处理进度输出
    fn execute_with_progress_args(
        &self,
        args: &[&str],
        progress_tx: Option<Sender<DismCmdProgress>>,
        operation_name: &str,
    ) -> Result<()> {
        log::info!(
            "[DismCmd] 执行: {} {}",
            self.dism_path.display(),
            args.join(" ")
        );

        let mut cmd = new_command(&self.dism_path);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        // 启动进程
        let mut child = cmd.spawn().context("启动 DISM 进程失败")?;

        // 读取并处理输出
        let result = self.process_output(&mut child, &progress_tx, operation_name);

        // 等待进程结束
        let status = child.wait().context("等待 DISM 进程失败")?;

        // 处理结果
        match result {
            Ok(_) => {
                if status.success() {
                    Self::send_progress(&progress_tx, 100, &format!("{}完成", operation_name));
                    Ok(())
                } else {
                    bail!("{}失败，退出代码: {:?}", operation_name, status.code())
                }
            }
            Err(e) => Err(e),
        }
    }

    /// 处理进程输出流
    fn process_output(
        &self,
        child: &mut Child,
        progress_tx: &Option<Sender<DismCmdProgress>>,
        operation_name: &str,
    ) -> Result<()> {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let mut error_output = String::new();
        let mut last_progress: u8 = 0;

        // 处理 stdout
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                if let Ok(line) = line_result {
                    // 尝试转换编码
                    let decoded_line = if line.is_ascii() {
                        line
                    } else {
                        gbk_to_utf8(line.as_bytes())
                    };

                    // 解析进度
                    if let Some(pct) = Self::parse_progress_line(&decoded_line) {
                        if pct != last_progress {
                            last_progress = pct;
                            Self::send_progress(
                                progress_tx,
                                pct,
                                &format!("{}中... {}%", operation_name, pct),
                            );
                        }
                    }

                    // 检测错误信息
                    if decoded_line.contains("Error")
                        || decoded_line.contains("错误")
                        || decoded_line.contains("失败")
                    {
                        error_output.push_str(&decoded_line);
                        error_output.push('\n');
                    }

                    // 打印日志
                    if !decoded_line.trim().is_empty() {
                        log::trace!("[DISM] {}", decoded_line);
                    }
                }
            }
        }

        // 处理 stderr
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            for line_result in reader.lines() {
                if let Ok(line) = line_result {
                    let decoded_line = if line.is_ascii() {
                        line
                    } else {
                        gbk_to_utf8(line.as_bytes())
                    };

                    if !decoded_line.trim().is_empty() {
                        error_output.push_str(&decoded_line);
                        error_output.push('\n');
                        log::trace!("[DISM ERR] {}", decoded_line);
                    }
                }
            }
        }

        Ok(())
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

    /// 解析 DISM 输出中的进度百分比
    fn parse_progress_line(line: &str) -> Option<u8> {
        // DISM 输出格式通常为: "[==== 25.0% ====]" 或 "25.0%"
        let line = line.trim();

        // 查找百分比数字
        if let Some(percent_pos) = line.find('%') {
            // 向前查找数字
            let before_percent = &line[..percent_pos];
            let number_start = before_percent
                .rfind(|c: char| !c.is_ascii_digit() && c != '.')
                .map(|i| i + 1)
                .unwrap_or(0);

            if let Ok(percentage) = before_percent[number_start..].parse::<f32>() {
                return Some((percentage as u8).min(100));
            }
        }

        None
    }

    /// 发送进度更新
    fn send_progress(tx: &Option<Sender<DismCmdProgress>>, percentage: u8, status: &str) {
        if let Some(ref tx) = tx {
            let _ = tx.send(DismCmdProgress {
                percentage,
                status: status.to_string(),
            });
        }
    }

    /// 查找目录中的所有 CAB 文件（递归）
    fn find_cab_files(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut cab_files = Vec::new();
        Self::find_cab_files_recursive(dir, &mut cab_files)?;
        Ok(cab_files)
    }

    /// 递归查找 CAB 文件
    fn find_cab_files_recursive(dir: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().to_lowercase() == "cab" {
                        result.push(path);
                    }
                }
            } else if path.is_dir() {
                Self::find_cab_files_recursive(&path, result)?;
            }
        }

        Ok(())
    }

    /// 分析目录内容（检查是否包含 INF 和 CAB 文件）
    fn analyze_directory(dir: &Path) -> (bool, bool) {
        let mut has_inf = false;
        let mut has_cab = false;

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        let ext_lower = ext.to_string_lossy().to_lowercase();
                        match ext_lower.as_str() {
                            "inf" => has_inf = true,
                            "cab" => has_cab = true,
                            _ => {}
                        }
                    }
                } else if path.is_dir() {
                    // 递归检查子目录
                    let (sub_inf, sub_cab) = Self::analyze_directory(&path);
                    has_inf = has_inf || sub_inf;
                    has_cab = has_cab || sub_cab;
                }

                // 如果两种都找到了，可以提前返回
                if has_inf && has_cab {
                    break;
                }
            }
        }

        (has_inf, has_cab)
    }
}

impl Default for DismCmd {
    fn default() -> Self {
        Self::new().expect("无法初始化 DISM 命令行执行器")
    }
}

// ============================================================================
// 便捷函数
// ============================================================================

/// 向离线系统添加驱动（便捷函数）
///
/// # 参数
/// - `image_path`: 离线映像路径（如 `D:\`）
/// - `driver_path`: 驱动路径
pub fn add_drivers_offline(image_path: &str, driver_path: &str) -> Result<()> {
    let dism = DismCmd::new()?;
    dism.add_drivers_from_directory(image_path, driver_path, None)
}

/// 向离线系统添加 CAB 包（便捷函数）
///
/// # 参数
/// - `image_path`: 离线映像路径
/// - `package_path`: CAB 包路径
pub fn add_package_offline(image_path: &str, package_path: &str) -> Result<()> {
    let dism = DismCmd::new()?;
    dism.add_package_offline(image_path, package_path, false, None)
}

/// 智能导入驱动和 CAB 包（便捷函数）
///
/// # 参数
/// - `image_path`: 离线映像路径
/// - `source_dir`: 源目录
pub fn import_drivers_smart(image_path: &str, source_dir: &str) -> Result<()> {
    let dism = DismCmd::new()?;
    dism.import_drivers_smart(image_path, source_dir, None)
}

/// 从离线系统导出驱动（便捷函数）
///
/// # 参数
/// - `image_path`: 离线映像路径
/// - `destination`: 导出目标目录
pub fn export_drivers_offline(image_path: &str, destination: &str) -> Result<()> {
    let dism = DismCmd::new()?;
    dism.export_drivers_offline(image_path, destination, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_progress() {
        assert_eq!(DismCmd::parse_progress_line("[==== 25.0% ====]"), Some(25));
        assert_eq!(DismCmd::parse_progress_line("50.0%"), Some(50));
        assert_eq!(DismCmd::parse_progress_line("Processing: 75%"), Some(75));
        assert_eq!(DismCmd::parse_progress_line("完成 100.0%"), Some(100));
        assert_eq!(DismCmd::parse_progress_line("No progress here"), None);
    }

    #[test]
    fn test_normalize_image_path() {
        assert_eq!(DismCmd::normalize_image_path("D:"), "D:\\");
        assert_eq!(DismCmd::normalize_image_path("D:\\"), "D:\\");
        assert_eq!(DismCmd::normalize_image_path("D:\\Windows"), "D:\\Windows\\");
        assert_eq!(
            DismCmd::normalize_image_path("  C:\\Test  "),
            "C:\\Test\\"
        );
    }

    #[test]
    fn test_ensure_scratch_directory() {
        // 这个测试会根据运行环境返回不同结果
        let scratch = DismCmd::ensure_scratch_directory();
        assert!(!scratch.is_empty());
    }
}
