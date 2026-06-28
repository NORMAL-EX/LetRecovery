//! 运行「程序目录\diskpart\」下的所有脚本（两端共享）。
//!
//! - `.cmd` / `.bat`：通过 `cmd /c` 执行（批处理）
//! - `.txt`：通过 `diskpart /s` 执行（diskpart 脚本）
//!
//! 其余扩展名忽略；按文件名排序依次执行。用于装机前的分区准备
//! （在 PE 中、格式化/释放镜像之前运行）。

use std::path::Path;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

/// 运行指定目录下的所有分区脚本，返回合并输出日志。
///
/// - 目录不存在或没有可执行脚本：返回 `Ok(提示信息)`。
/// - 任一脚本进程无法启动或返回非 0 退出码：返回 `Err(已收集日志)`。
pub fn run_scripts_in_dir(dir: &Path) -> Result<String, String> {
    if !dir.exists() {
        return Ok(format!("diskpart 脚本目录不存在，跳过：{}", dir.display()));
    }

    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("读取脚本目录失败：{}", e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    let mut log = String::new();
    let mut any = false;

    for path in entries {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let path_str = path.to_string_lossy().into_owned();
        let (program, args): (&str, Vec<String>) = match ext.as_str() {
            "cmd" | "bat" => ("cmd", vec!["/c".into(), path_str.clone()]),
            "txt" => ("diskpart", vec!["/s".into(), path_str.clone()]),
            _ => continue,
        };

        any = true;
        log.push_str(&format!("\n>>> 执行脚本: {}\n", path.display()));
        match new_command(program).args(&args).output() {
            Ok(out) => {
                let so = gbk_to_utf8(&out.stdout);
                let se = gbk_to_utf8(&out.stderr);
                if !so.trim().is_empty() {
                    log.push_str(so.trim());
                    log.push('\n');
                }
                if !se.trim().is_empty() {
                    log.push_str(se.trim());
                    log.push('\n');
                }
                if !out.status.success() {
                    log.push_str(&format!("[脚本返回非 0 退出码] {}\n", path.display()));
                    return Err(log);
                }
            }
            Err(e) => {
                log.push_str(&format!("[无法启动 {}] {}\n", program, e));
                return Err(log);
            }
        }
    }

    if !any {
        log.push_str(&format!(
            "目录中没有可执行脚本(.cmd/.bat/.txt)：{}",
            dir.display()
        ));
    }
    Ok(log)
}
