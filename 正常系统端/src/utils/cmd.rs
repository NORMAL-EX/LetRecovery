use std::ffi::OsStr;
use std::process::{Child, Command, Output, Stdio};

use crate::utils::encoding::gbk_to_utf8;

/// 创建一个配置好的 Command，在 Windows 上隐藏控制台窗口
pub fn create_command<S: AsRef<OsStr>>(program: S) -> Command {
    lr_core::command::new_command(program)
}

/// 执行命令并在 debug 模式下输出调试信息
pub fn run_command<S: AsRef<OsStr>>(program: S, args: &[&str]) -> std::io::Result<Output> {
    #[cfg(debug_assertions)]
    {
        log::debug!(
            "[CMD] {} {}",
            program.as_ref().to_string_lossy(),
            args.join(" ")
        );
    }

    let output = create_command(program).args(args).output()?;

    #[cfg(debug_assertions)]
    {
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);

        if !stdout.trim().is_empty() {
            log::debug!("[STDOUT] {}", stdout.trim());
        }
        if !stderr.trim().is_empty() {
            log::debug!("[STDERR] {}", stderr.trim());
        }
        log::debug!("[EXIT] {}", output.status);
        log::debug!("---");
    }

    Ok(output)
}

/// 执行命令并spawn（不等待结果）
pub fn spawn_command<S: AsRef<OsStr>>(program: S, args: &[&str]) -> std::io::Result<Child> {
    #[cfg(debug_assertions)]
    {
        log::debug!(
            "[SPAWN] {} {}",
            program.as_ref().to_string_lossy(),
            args.join(" ")
        );
    }

    create_command(program).args(args).spawn()
}

/// 执行命令并返回 stdout 字符串
pub fn run_command_string<S: AsRef<OsStr>>(program: S, args: &[&str]) -> std::io::Result<String> {
    let output = run_command(program, args)?;
    Ok(gbk_to_utf8(&output.stdout))
}

/// 执行命令并返回 stdout 字符串（带自定义参数的版本）
pub fn run_command_with_args<S: AsRef<OsStr>>(
    program: S,
    args: Vec<String>,
) -> std::io::Result<Output> {
    #[cfg(debug_assertions)]
    {
        log::debug!(
            "[CMD] {} {}",
            program.as_ref().to_string_lossy(),
            args.join(" ")
        );
    }

    let output = create_command(program).args(&args).output()?;

    #[cfg(debug_assertions)]
    {
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);

        if !stdout.trim().is_empty() {
            log::debug!("[STDOUT] {}", stdout.trim());
        }
        if !stderr.trim().is_empty() {
            log::debug!("[STDERR] {}", stderr.trim());
        }
        log::debug!("[EXIT] {}", output.status);
        log::debug!("---");
    }

    Ok(output)
}

/// 执行带 Stdio 管道的命令（用于 DISM 等需要实时输出的场景）
pub fn spawn_command_piped<S: AsRef<OsStr>>(program: S, args: &[&str]) -> std::io::Result<Child> {
    #[cfg(debug_assertions)]
    {
        log::debug!(
            "[SPAWN PIPED] {} {}",
            program.as_ref().to_string_lossy(),
            args.join(" ")
        );
    }

    create_command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}
