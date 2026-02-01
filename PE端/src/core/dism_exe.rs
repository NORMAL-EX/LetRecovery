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
use std::sync::mpsc::Sender;

use anyhow::{bail, Context, Result};

use crate::utils::encoding::gbk_to_utf8;

/// Windows CREATE_NO_WINDOW 标志，用于隐藏控制台窗口
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// DISM 操作进度
#[derive(Debug, Clone)]
pub struct DismExeProgress {
    pub percentage: u8,
    pub status: String,
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
            let system_path = PathBuf::from(&system_root).join("System32").join("dism.exe");
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
            "无法找到 dism.exe。请确保在 PE 环境或 Windows 系统中运行。\n\
             已搜索的路径:\n\
             - X:\\Windows\\System32\\dism.exe (PE 环境)\n\
             - C:\\Windows\\System32\\dism.exe (Windows 系统)"
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
        log::info!("[DISM.EXE] 执行: {} {}", self.dism_path.display(), args.join(" "));

        let mut child = self
            .create_command()
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("启动 dism.exe 失败")?;

        let stdout = child.stdout.take().context("无法获取 stdout")?;
        let stderr = child.stderr.take().context("无法获取 stderr")?;

        // 读取并解析 stdout
        let progress_tx_clone = progress_tx.clone();
        let stdout_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let mut output = String::new();

            for line_result in reader.lines() {
                if let Ok(line) = line_result {
                    // 转换编码（Windows 可能使用 GBK）
                    let decoded_line = if line.is_ascii() {
                        line
                    } else {
                        gbk_to_utf8(line.as_bytes())
                    };

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
            }

            output
        });

        // 读取 stderr
        let stderr_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut error_output = String::new();

            for line_result in reader.lines() {
                if let Ok(line) = line_result {
                    let decoded_line = if line.is_ascii() {
                        line
                    } else {
                        gbk_to_utf8(line.as_bytes())
                    };

                    error_output.push_str(&decoded_line);
                    error_output.push('\n');

                    log::trace!("[DISM.EXE STDERR] {}", decoded_line);
                }
            }

            error_output
        });

        // 等待进程完成
        let status = child.wait().context("等待 dism.exe 完成失败")?;

        // 获取输出
        let stdout_text = stdout_handle.join().unwrap_or_default();
        let stderr_text = stderr_handle.join().unwrap_or_default();

        // 发送完成进度
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismExeProgress {
                percentage: 100,
                status: "完成".to_string(),
            });
        }

        if !status.success() {
            let error_msg = if !stderr_text.trim().is_empty() {
                stderr_text.trim().to_string()
            } else if !stdout_text.trim().is_empty() {
                // DISM 有时会将错误信息输出到 stdout
                Self::extract_error_from_output(&stdout_text)
            } else {
                format!("dism.exe 退出码: {:?}", status.code())
            };

            bail!("DISM 操作失败: {}", error_msg);
        }

        log::info!("[DISM.EXE] 操作成功完成");
        Ok(stdout_text)
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
                    status: format!("处理中 {}%", pct),
                });
            }
        }

        // 检查特定状态文本
        let lower = trimmed.to_lowercase();
        if lower.contains("完成") || lower.contains("successfully") || lower.contains("success") {
            return Some(DismExeProgress {
                percentage: 100,
                status: "完成".to_string(),
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
        log::info!(
            "[DISM.EXE] 添加驱动到离线系统: {} -> {}",
            driver_path,
            image_path
        );

        // 验证路径
        let driver_path_obj = Path::new(driver_path);
        if !driver_path_obj.exists() {
            bail!("驱动路径不存在: {}", driver_path);
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

        if force_unsigned {
            args.push("/ForceUnsigned".to_string());
        }

        // 转换为 &str 切片
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        self.execute_with_progress(&args_ref, progress_tx)?;
        Ok(())
    }

    /// 批量添加驱动到离线系统镜像
    ///
    /// 递归搜索指定目录下的所有驱动并添加到离线系统。
    ///
    /// # 参数
    /// - `image_path`: 离线系统根目录
    /// - `driver_dirs`: 驱动目录列表
    /// - `progress_tx`: 进度通道（可选）
    ///
    /// # 返回
    /// - (成功数, 失败数)
    pub fn add_drivers_batch(
        &self,
        image_path: &str,
        driver_dirs: &[&str],
        progress_tx: Option<Sender<DismExeProgress>>,
    ) -> Result<(usize, usize)> {
        let total = driver_dirs.len();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (index, driver_dir) in driver_dirs.iter().enumerate() {
            // 发送当前进度
            if let Some(ref tx) = progress_tx {
                let overall_pct = ((index * 100) / total.max(1)) as u8;
                let _ = tx.send(DismExeProgress {
                    percentage: overall_pct,
                    status: format!("添加驱动 {}/{}", index + 1, total),
                });
            }

            match self.add_driver_offline(image_path, driver_dir, true, false, None) {
                Ok(_) => {
                    success_count += 1;
                    log::info!("[DISM.EXE] 驱动添加成功: {}", driver_dir);
                }
                Err(e) => {
                    fail_count += 1;
                    log::warn!("[DISM.EXE] 驱动添加失败: {} - {}", driver_dir, e);
                }
            }
        }

        // 发送完成进度
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismExeProgress {
                percentage: 100,
                status: format!("完成: {} 成功, {} 失败", success_count, fail_count),
            });
        }

        log::info!(
            "[DISM.EXE] 批量驱动添加完成: 成功 {}, 失败 {}",
            success_count,
            fail_count
        );

        Ok((success_count, fail_count))
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
            bail!("CAB 包文件不存在: {}", package_path);
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
                    status: format!("安装更新 {}/{}", index + 1, total),
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
                status: format!("完成: {} 成功, {} 失败", success_count, fail_count),
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

    // =========================================================================
    // 公共 API - 信息查询
    // =========================================================================

    /// 获取离线系统镜像信息
    ///
    /// 使用 dism.exe /Get-ImageInfo 获取镜像信息
    pub fn get_image_info(&self, image_path: &str) -> Result<String> {
        let normalized = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };

        let args = [
            &format!("/Image:{}", normalized),
            "/Get-CurrentEdition",
        ];

        self.execute_with_progress(&args, None)
    }

    /// 获取离线系统中已安装的驱动列表
    pub fn get_drivers(&self, image_path: &str) -> Result<String> {
        let normalized = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };

        let args = [
            &format!("/Image:{}", normalized),
            "/Get-Drivers",
        ];

        self.execute_with_progress(&args, None)
    }

    /// 获取离线系统中已安装的更新包列表
    pub fn get_packages(&self, image_path: &str) -> Result<String> {
        let normalized = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };

        let args = [
            &format!("/Image:{}", normalized),
            "/Get-Packages",
        ];

        self.execute_with_progress(&args, None)
    }
}

impl Default for DismExe {
    fn default() -> Self {
        Self::new().expect("无法创建 DismExe 实例")
    }
}

// =============================================================================
// 便捷函数
// =============================================================================

/// 添加驱动到离线系统镜像（便捷函数）
///
/// # 参数
/// - `image_path`: 离线系统根目录
/// - `driver_path`: 驱动目录或 INF 文件路径
/// - `progress_tx`: 进度通道（可选）
pub fn add_drivers_to_offline_image(
    image_path: &str,
    driver_path: &str,
    progress_tx: Option<Sender<DismExeProgress>>,
) -> Result<()> {
    let dism = DismExe::new()?;
    dism.add_driver_offline(image_path, driver_path, true, false, progress_tx)
}

/// 添加 CAB 包到离线系统镜像（便捷函数）
///
/// # 参数
/// - `image_path`: 离线系统根目录
/// - `cab_path`: CAB 包文件路径
/// - `progress_tx`: 进度通道（可选）
pub fn add_package_to_offline_image(
    image_path: &str,
    cab_path: &str,
    progress_tx: Option<Sender<DismExeProgress>>,
) -> Result<()> {
    let dism = DismExe::new()?;
    dism.add_package_offline(image_path, cab_path, false, progress_tx)
}

/// 从目录批量安装 CAB 包到离线系统镜像（便捷函数）
///
/// # 参数
/// - `image_path`: 离线系统根目录
/// - `cab_dir`: 包含 CAB 文件的目录
/// - `progress_tx`: 进度通道（可选）
///
/// # 返回
/// - (成功数, 失败数)
pub fn add_packages_from_directory(
    image_path: &str,
    cab_dir: &Path,
    progress_tx: Option<Sender<DismExeProgress>>,
) -> Result<(usize, usize)> {
    let dism = DismExe::new()?;
    dism.add_packages_from_directory(image_path, cab_dir, progress_tx)
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
}
