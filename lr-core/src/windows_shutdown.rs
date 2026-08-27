//! Scheduled local shutdown/restart through the documented shutdown API.

#[cfg(windows)]
fn schedule_power_action(
    timeout_seconds: u32,
    message: &str,
    force_apps_closed: bool,
    reboot_after_shutdown: bool,
) -> anyhow::Result<()> {
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
            force_apps_closed,
            reboot_after_shutdown,
            reason,
        )
        .context("InitiateSystemShutdownExW")
    }
}

/// Schedule a planned local restart. Applications are not forcibly closed; the documented API
/// may therefore wait for an interactive application to finish protecting unsaved work.
#[cfg(windows)]
pub fn schedule_restart(timeout_seconds: u32, message: &str) -> anyhow::Result<()> {
    schedule_power_action(timeout_seconds, message, false, true)
}

/// Schedule a planned restart for an explicitly unattended environment such as WinPE or a
/// pre-desktop first-logon transition. Unlike the interactive restart boundary above, this opts
/// into closing applications so a shell/helper process without user-authored data cannot
/// indefinitely block the requested restart. Callers must finish and read back all durable state
/// before entering this boundary.
#[cfg(windows)]
pub fn schedule_restart_for_automation(timeout_seconds: u32, message: &str) -> anyhow::Result<()> {
    schedule_power_action(timeout_seconds, message, true, true)
}

/// Schedule a planned local power-off while allowing the interactive session and User Profile
/// Service to close normally. This is used by first-logon automation after writing user files;
/// Microsoft's contract explicitly warns that forcing applications closed can lose data.
#[cfg(windows)]
pub fn schedule_graceful_shutdown(timeout_seconds: u32, message: &str) -> anyhow::Result<()> {
    schedule_power_action(timeout_seconds, message, false, false)
}

/// Schedule a planned local power-off for an explicitly unattended automation run. The force flag
/// is enabled only for this opt-in path so a disposable VM cannot remain blocked by an invisible
/// first-logon process after all LetRecovery terminal diagnostics have been flushed.
#[cfg(windows)]
pub fn schedule_shutdown(timeout_seconds: u32, message: &str) -> anyhow::Result<()> {
    schedule_power_action(timeout_seconds, message, true, false)
}

#[cfg(not(windows))]
pub fn schedule_restart(_timeout_seconds: u32, _message: &str) -> anyhow::Result<()> {
    anyhow::bail!("Windows shutdown APIs are unavailable on this platform")
}

#[cfg(not(windows))]
pub fn schedule_restart_for_automation(
    _timeout_seconds: u32,
    _message: &str,
) -> anyhow::Result<()> {
    anyhow::bail!("Windows shutdown APIs are unavailable on this platform")
}

#[cfg(not(windows))]
pub fn schedule_graceful_shutdown(_timeout_seconds: u32, _message: &str) -> anyhow::Result<()> {
    anyhow::bail!("Windows shutdown APIs are unavailable on this platform")
}

#[cfg(not(windows))]
pub fn schedule_shutdown(_timeout_seconds: u32, _message: &str) -> anyhow::Result<()> {
    anyhow::bail!("Windows shutdown APIs are unavailable on this platform")
}
