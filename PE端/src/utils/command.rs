//! 命令执行辅助模块
//!
//! 提供隐藏控制台窗口的 Command 创建函数

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows CREATE_NO_WINDOW 标志
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 创建一个隐藏控制台窗口的 Command
///
/// 在 Windows 上设置 CREATE_NO_WINDOW 标志以防止弹出控制台窗口
/// 在其他平台上返回普通的 Command
pub fn new_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);

    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd
}
