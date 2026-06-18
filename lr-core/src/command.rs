//! 命令执行辅助：创建隐藏控制台窗口的 Command（两端共享）。

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows CREATE_NO_WINDOW 标志
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 创建一个隐藏控制台窗口的 Command。
///
/// 在 Windows 上设置 CREATE_NO_WINDOW 防止弹出控制台窗口；其它平台返回普通 Command。
pub fn new_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);

    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn keeps_program_name() {
        let cmd = new_command("reg.exe");
        assert_eq!(cmd.get_program(), OsStr::new("reg.exe"));
    }
}
