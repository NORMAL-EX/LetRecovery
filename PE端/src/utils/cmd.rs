use std::ffi::OsStr;
use std::process::Command;

/// Windows CREATE_NO_WINDOW 标志
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 创建一个配置好的 Command，在 Windows 上隐藏控制台窗口
pub fn create_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd
}
