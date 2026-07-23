use std::ffi::OsStr;
use std::process::Command;

/// 创建一个配置好的 Command，在 Windows 上隐藏控制台窗口
#[allow(dead_code, reason = "compatibility re-export for custom PE modules")]
pub fn create_command<S: AsRef<OsStr>>(program: S) -> Command {
    lr_core::command::new_command(program)
}
