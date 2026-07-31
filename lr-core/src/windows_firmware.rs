//! Documented firmware-mode probe shared by the desktop and WinPE endpoints.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareType {
    Bios,
    Uefi,
}

#[cfg(windows)]
pub fn detect_firmware_type() -> anyhow::Result<FirmwareType> {
    use anyhow::bail;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        GetLastError, SetLastError, ERROR_INVALID_FUNCTION, ERROR_SUCCESS,
    };
    use windows::Win32::System::WindowsProgramming::GetFirmwareEnvironmentVariableW;

    // Microsoft documents this dummy empty-name/zero-GUID call as the
    // firmware-type test: legacy BIOS returns ERROR_INVALID_FUNCTION; UEFI
    // returns a firmware-variable error instead.
    let empty_name = [0_u16];
    let zero_guid: Vec<u16> = "{00000000-0000-0000-0000-000000000000}"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        SetLastError(ERROR_SUCCESS);
        let result = GetFirmwareEnvironmentVariableW(
            PCWSTR(empty_name.as_ptr()),
            PCWSTR(zero_guid.as_ptr()),
            None,
            0,
        );
        if result != 0 {
            return Ok(FirmwareType::Uefi);
        }
        let status = GetLastError();
        if status == ERROR_INVALID_FUNCTION {
            return Ok(FirmwareType::Bios);
        }
        if status == ERROR_SUCCESS {
            bail!("firmware probe returned zero without a Win32 error");
        }
        Ok(FirmwareType::Uefi)
    }
}

#[cfg(not(windows))]
pub fn detect_firmware_type() -> anyhow::Result<FirmwareType> {
    anyhow::bail!("Windows firmware APIs are unavailable on this platform")
}
