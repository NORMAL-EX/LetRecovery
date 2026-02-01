//! Windows Cabinet (.cab) 文件解压模块
//!
//! 使用 expand.exe 命令行工具实现 .cab 文件解压。
//! 主要用于解压 Windows 更新包（如 KB2990941、KB3087873 等 NVMe 驱动补丁）。
//!
//! 此实现不依赖 Windows SetupAPI，而是使用系统自带的 expand.exe 命令，
//! 具有更好的兼容性和稳定性。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};

use crate::utils::command::new_command;

/// Cabinet 文件解压器
/// 
/// 使用 Windows expand.exe 命令行工具解压 .cab 文件。
/// 支持单文件解压和批量解压。
pub struct CabinetExtractor {
    expand_path: PathBuf,
}

impl CabinetExtractor {
    /// 创建 Cabinet 解压器实例
    /// 
    /// 会自动查找系统中的 expand.exe
    pub fn new() -> Result<Self> {
        let expand_path = Self::find_expand_executable()?;
        println!("[CABINET] 使用 expand: {}", expand_path.display());
        Ok(Self { expand_path })
    }

    /// 查找 expand.exe 可执行文件
    fn find_expand_executable() -> Result<PathBuf> {
        // 优先尝试 System32 目录
        if let Ok(windir) = std::env::var("WINDIR") {
            let system32_expand = PathBuf::from(&windir)
                .join("System32")
                .join("expand.exe");
            if system32_expand.exists() {
                return Ok(system32_expand);
            }
        }

        // 尝试直接使用 expand.exe（依赖 PATH）
        let expand = PathBuf::from("expand.exe");
        if Self::verify_expand_available(&expand) {
            return Ok(expand);
        }

        bail!("未找到 expand.exe，请确保 Windows 系统完整")
    }

    /// 验证 expand.exe 是否可用
    fn verify_expand_available(expand_path: &Path) -> bool {
        new_command(expand_path)
            .arg("-?")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// 解压 .cab 文件到指定目录
    ///
    /// # 参数
    /// - `cab_path`: .cab 文件路径
    /// - `dest_dir`: 目标目录
    ///
    /// # 返回
    /// - 成功解压的文件列表
    pub fn extract(&self, cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
        // 验证 cab 文件存在
        if !cab_path.exists() {
            bail!("CAB 文件不存在: {}", cab_path.display());
        }

        // 确保目标目录存在
        std::fs::create_dir_all(dest_dir)
            .context("创建目标目录失败")?;

        println!(
            "[CABINET] 解压: {} -> {}",
            cab_path.display(),
            dest_dir.display()
        );

        // 使用 expand.exe 解压
        // 命令格式: expand.exe -F:* <cab_file> <dest_dir>
        let mut cmd = new_command(&self.expand_path);
        cmd.arg("-F:*")
            .arg(cab_path)
            .arg(dest_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().context("执行 expand.exe 失败")?;

        // 处理输出
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            let error_msg = if !stderr.is_empty() {
                stderr.to_string()
            } else {
                stdout.to_string()
            };
            bail!("expand.exe 解压失败: {}", error_msg.trim());
        }

        // 解析输出，获取解压的文件列表
        let extracted_files = self.parse_expand_output(&stdout, dest_dir);

        // 如果无法从输出解析文件列表，则扫描目标目录
        let files = if extracted_files.is_empty() {
            self.scan_extracted_files(dest_dir)?
        } else {
            extracted_files
        };

        println!("[CABINET] 成功解压 {} 个文件", files.len());

        Ok(files)
    }

    /// 解析 expand.exe 的输出，提取解压的文件列表
    fn parse_expand_output(&self, output: &str, dest_dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();

        // expand.exe 输出格式类似:
        // "正在展开: file1.inf"
        // "Expanding: file2.sys"
        for line in output.lines() {
            let line = line.trim();
            
            // 跳过空行和非文件行
            if line.is_empty() {
                continue;
            }

            // 尝试提取文件名
            let file_name = if let Some(idx) = line.rfind(':') {
                line[idx + 1..].trim()
            } else if line.contains('.') && !line.contains(' ') {
                line
            } else {
                continue;
            };

            if !file_name.is_empty() && file_name.contains('.') {
                let file_path = dest_dir.join(file_name);
                if file_path.exists() {
                    files.push(file_path);
                }
            }
        }

        files
    }

    /// 扫描目标目录获取所有文件
    fn scan_extracted_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        self.scan_dir_recursive(dir, &mut files)?;
        Ok(files)
    }

    /// 递归扫描目录
    fn scan_dir_recursive(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                files.push(path);
            } else if path.is_dir() {
                self.scan_dir_recursive(&path, files)?;
            }
        }

        Ok(())
    }

    /// 列出 .cab 文件中的内容（不解压）
    ///
    /// # 参数
    /// - `cab_path`: .cab 文件路径
    ///
    /// # 返回
    /// - 文件名列表
    pub fn list_contents(&self, cab_path: &Path) -> Result<Vec<String>> {
        if !cab_path.exists() {
            bail!("CAB 文件不存在: {}", cab_path.display());
        }

        // 使用 expand.exe -D 列出内容
        let mut cmd = new_command(&self.expand_path);
        cmd.arg("-D")
            .arg(cab_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().context("执行 expand.exe 失败")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("列出 CAB 内容失败: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut files = Vec::new();

        for line in stdout.lines() {
            let line = line.trim();
            // expand -D 输出每行一个文件名
            if !line.is_empty() && line.contains('.') {
                files.push(line.to_string());
            }
        }

        Ok(files)
    }

    /// 检查文件是否为 .cab 文件
    pub fn is_cab_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("cab"))
            .unwrap_or(false)
    }

    /// 检查文件是否为有效的 CAB 文件（通过文件头）
    pub fn is_valid_cab_file(path: &Path) -> bool {
        if !path.exists() || !path.is_file() {
            return false;
        }

        // CAB 文件的魔数是 "MSCF" (0x4D534346)
        if let Ok(mut file) = std::fs::File::open(path) {
            use std::io::Read;
            let mut magic = [0u8; 4];
            if file.read_exact(&mut magic).is_ok() {
                return &magic == b"MSCF";
            }
        }

        false
    }
}

// ============================================================================
// 便捷函数
// ============================================================================

/// 解压 .cab 文件到指定目录
///
/// # 参数
/// - `cab_path`: .cab 文件路径
/// - `dest_dir`: 目标目录
///
/// # 返回
/// - 成功解压的文件列表
pub fn extract_cab(cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    let extractor = CabinetExtractor::new()?;
    extractor.extract(cab_path, dest_dir)
}

/// 解压目录中的所有 .cab 文件
///
/// # 参数
/// - `source_dir`: 包含 .cab 文件的源目录
/// - `dest_dir`: 目标目录（每个 cab 会解压到以 cab 文件名命名的子目录）
///
/// # 返回
/// - 成功解压的 cab 文件数量
pub fn extract_all_cabs(source_dir: &Path, dest_dir: &Path) -> Result<usize> {
    let extractor = CabinetExtractor::new()?;
    let mut count = 0;

    // 确保目标目录存在
    std::fs::create_dir_all(dest_dir)?;

    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();

        if CabinetExtractor::is_cab_file(&path) {
            let cab_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");

            let cab_dest = dest_dir.join(cab_name);

            match extractor.extract(&path, &cab_dest) {
                Ok(files) => {
                    println!(
                        "[CABINET] 解压 {:?}: {} 个文件",
                        path.file_name(),
                        files.len()
                    );
                    count += 1;
                }
                Err(e) => {
                    println!("[CABINET] 解压 {:?} 失败: {}", path.file_name(), e);
                }
            }
        }
    }

    Ok(count)
}

/// 查找目录中的所有 .cab 文件
pub fn find_cab_files(dir: &Path) -> Vec<PathBuf> {
    let mut cab_files = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if CabinetExtractor::is_cab_file(&path) {
                cab_files.push(path);
            }
        }
    }

    cab_files
}

/// 递归查找目录中的所有 .cab 文件
pub fn find_cab_files_recursive(dir: &Path) -> Vec<PathBuf> {
    let mut cab_files = Vec::new();
    find_cab_files_recursive_inner(dir, &mut cab_files);
    cab_files
}

fn find_cab_files_recursive_inner(dir: &Path, result: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && CabinetExtractor::is_cab_file(&path) {
                result.push(path);
            } else if path.is_dir() {
                find_cab_files_recursive_inner(&path, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_cab_file() {
        assert!(CabinetExtractor::is_cab_file(Path::new("test.cab")));
        assert!(CabinetExtractor::is_cab_file(Path::new("test.CAB")));
        assert!(CabinetExtractor::is_cab_file(Path::new(
            "Windows6.1-KB2990941-v3-x64.cab"
        )));
        assert!(!CabinetExtractor::is_cab_file(Path::new("test.inf")));
        assert!(!CabinetExtractor::is_cab_file(Path::new("test.sys")));
    }
}
