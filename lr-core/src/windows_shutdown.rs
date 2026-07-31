//! Scheduled local restart through the documented shutdown API.

#[cfg(windows)]
pub fn schedule_restart(timeout_seconds: u32, message: &str) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, HANDLE,
        LUID,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Shutdown::{
        InitiateSystemShutdownExW, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION,
        SHTDN_REASON_MINOR_INSTALLATION, SHUTDOWN_REASON,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct OwnedToken(HANDLE);
    impl Drop for OwnedToken {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    if message.contains('\0') {
        bail!("shutdown message contains NUL");
    }
    let mut raw_token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut raw_token,
        )
        .context("OpenProcessToken for SeShutdownPrivilege")?;
    }
    let token = OwnedToken(raw_token);
    let privilege_name: Vec<u16> = "SeShutdownPrivilege"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut luid = LUID::default();
    unsafe {
        LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(privilege_name.as_ptr()), &mut luid)
            .context("LookupPrivilegeValueW(SeShutdownPrivilege)")?;
    }
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    unsafe {
        SetLastError(ERROR_SUCCESS);
        AdjustTokenPrivileges(token.0, false, Some(&privileges), 0, None, None)
            .context("AdjustTokenPrivileges(SeShutdownPrivilege)")?;
        let status = GetLastError();
        if status == ERROR_NOT_ALL_ASSIGNED {
            bail!("the process token does not contain SeShutdownPrivilege");
        }
        if status != ERROR_SUCCESS {
            bail!(
                "AdjustTokenPrivileges(SeShutdownPrivilege) returned Win32 error {}",
                status.0
            );
        }
    }

    let message_wide: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let reason = SHUTDOWN_REASON(
        SHTDN_REASON_MAJOR_APPLICATION.0
            | SHTDN_REASON_MINOR_INSTALLATION.0
            | SHTDN_REASON_FLAG_PLANNED.0,
    );
    unsafe {
        InitiateSystemShutdownExW(
            PCWSTR::null(),
            PCWSTR(message_wide.as_ptr()),
            timeout_seconds,
            false,
            true,
            reason,
        )
        .context("InitiateSystemShutdownExW")
    }
}

#[cfg(not(windows))]
pub fn schedule_restart(_timeout_seconds: u32, _message: &str) -> anyhow::Result<()> {
    anyhow::bail!("Windows shutdown APIs are unavailable on this platform")
}
