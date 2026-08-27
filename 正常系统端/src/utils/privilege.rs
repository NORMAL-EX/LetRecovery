use anyhow::Result;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 检查当前进程是否具有管理员权限
pub fn is_admin() -> bool {
    unsafe {
        let mut token_handle = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;

        let result = GetTokenInformation(
            token_handle,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size,
            &mut size,
        );

        let _ = CloseHandle(token_handle);

        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// 以管理员权限重新启动程序
/// Elevate only a plain GUI launch. CLI routing must never call this helper, and no command-line
/// arguments are forwarded across the elevation boundary.
pub fn restart_gui_as_admin() -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe_path = std::env::current_exe()?;
    let exe_path_wide: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let operation: Vec<u16> = "runas\0".encode_utf16().collect();

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(exe_path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW returns an HINSTANCE-compatible error code in the range 0..=32.
    // Do not terminate the current process when elevation was cancelled or failed.
    if result.0 as isize <= 32 {
        anyhow::bail!(
            "ShellExecuteW(runas) failed with code {}",
            result.0 as isize
        );
    }

    Ok(())
}
