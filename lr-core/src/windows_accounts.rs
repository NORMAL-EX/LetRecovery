//! Documented NetAPI boundary for local-account inventory and updates.
//!
//! Account enumeration uses `NetUserEnum` level 1. Password and flag changes
//! use separate `NetUserSetInfo` calls (levels 1003 and 1008) so unrelated
//! account fields are never reset.

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAccount {
    pub name: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountUpdateError {
    InvalidAccount,
    NotFound(String),
    Inventory(String),
    Password(String),
    /// The password was already cleared, but enabling the account failed.
    EnableAfterPasswordCleared(String),
}

impl fmt::Display for AccountUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAccount => formatter.write_str("invalid local account name"),
            Self::NotFound(name) => write!(formatter, "local account not found: {name}"),
            Self::Inventory(detail) => {
                write!(formatter, "failed to enumerate local accounts: {detail}")
            }
            Self::Password(detail) => write!(
                formatter,
                "failed to clear local account password: {detail}"
            ),
            Self::EnableAfterPasswordCleared(detail) => {
                write!(
                    formatter,
                    "password was cleared, but enabling the account failed: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for AccountUpdateError {}

pub fn validate_account_name(account: &str) -> Result<&str, AccountUpdateError> {
    let account = account.trim();
    if account.is_empty()
        || account.encode_utf16().count() > 256
        || account.chars().any(|character| character.is_control())
    {
        return Err(AccountUpdateError::InvalidAccount);
    }
    Ok(account)
}

pub const fn enabled_flags(flags: u32) -> u32 {
    flags & !0x0000_0002
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr;
    use std::slice;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_MORE_DATA;
    use windows::Win32::NetworkManagement::NetManagement::{
        NERR_Success, NERR_UserNotFound, NetApiBufferFree, NetUserEnum, NetUserGetInfo,
        NetUserSetInfo, FILTER_NORMAL_ACCOUNT, MAX_PREFERRED_LENGTH, UF_ACCOUNTDISABLE,
        USER_INFO_1, USER_INFO_1003, USER_INFO_1008,
    };

    use super::{enabled_flags, validate_account_name, AccountUpdateError, LocalAccount};

    struct NetBuffer(*mut u8);

    impl Drop for NetBuffer {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = NetApiBufferFree(Some(self.0.cast::<c_void>()));
                }
            }
        }
    }

    fn api_error(operation: &str, status: u32, parameter: u32) -> String {
        if parameter == 0 {
            format!("{operation} returned NET_API_STATUS {status}")
        } else {
            format!("{operation} returned NET_API_STATUS {status} for parameter {parameter}")
        }
    }

    unsafe fn pwstr_to_string(value: PWSTR) -> Result<String, AccountUpdateError> {
        value
            .to_string()
            .map_err(|error| AccountUpdateError::Inventory(error.to_string()))
    }

    pub fn list_local_accounts() -> Result<Vec<LocalAccount>, AccountUpdateError> {
        let mut accounts = Vec::new();
        let mut resume = 0_u32;

        loop {
            let mut raw = ptr::null_mut::<u8>();
            let mut entries_read = 0_u32;
            let mut total_entries = 0_u32;
            let status = unsafe {
                NetUserEnum(
                    PCWSTR::null(),
                    1,
                    FILTER_NORMAL_ACCOUNT,
                    &mut raw,
                    MAX_PREFERRED_LENGTH,
                    &mut entries_read,
                    &mut total_entries,
                    Some(&mut resume),
                )
            };
            let buffer = NetBuffer(raw);
            if status != NERR_Success && status != ERROR_MORE_DATA.0 {
                return Err(AccountUpdateError::Inventory(api_error(
                    "NetUserEnum",
                    status,
                    0,
                )));
            }
            if entries_read != 0 {
                if buffer.0.is_null() {
                    return Err(AccountUpdateError::Inventory(
                        "NetUserEnum returned entries with a null buffer".to_owned(),
                    ));
                }
                let entries = unsafe {
                    slice::from_raw_parts(buffer.0.cast::<USER_INFO_1>(), entries_read as usize)
                };
                for entry in entries {
                    let name = unsafe { pwstr_to_string(entry.usri1_name)? };
                    if !name.is_empty() {
                        accounts.push(LocalAccount {
                            name,
                            disabled: entry.usri1_flags.0 & UF_ACCOUNTDISABLE.0 != 0,
                        });
                    }
                }
            }
            if status == NERR_Success {
                break;
            }
        }

        Ok(accounts)
    }

    unsafe fn get_account_flags(account: PCWSTR) -> Result<u32, AccountUpdateError> {
        let mut raw = ptr::null_mut::<u8>();
        let status = NetUserGetInfo(PCWSTR::null(), account, 1, &mut raw);
        let buffer = NetBuffer(raw);
        if status == NERR_UserNotFound {
            return Err(AccountUpdateError::NotFound(
                account.to_string().unwrap_or_default(),
            ));
        }
        if status != NERR_Success {
            return Err(AccountUpdateError::Inventory(api_error(
                "NetUserGetInfo",
                status,
                0,
            )));
        }
        if buffer.0.is_null() {
            return Err(AccountUpdateError::Inventory(
                "NetUserGetInfo returned a null buffer".to_owned(),
            ));
        }
        Ok((*buffer.0.cast::<USER_INFO_1>()).usri1_flags.0)
    }

    pub fn clear_password_and_enable(account: &str) -> Result<(), AccountUpdateError> {
        let account = validate_account_name(account)?;
        let account_wide: Vec<u16> = account.encode_utf16().chain(Some(0)).collect();
        let account_pcwstr = PCWSTR(account_wide.as_ptr());
        let current_flags = unsafe { get_account_flags(account_pcwstr)? };

        let mut empty_password = [0_u16];
        let password_info = USER_INFO_1003 {
            usri1003_password: PWSTR(empty_password.as_mut_ptr()),
        };
        let mut parameter_error = 0_u32;
        let password_status = unsafe {
            NetUserSetInfo(
                PCWSTR::null(),
                account_pcwstr,
                1003,
                (&password_info as *const USER_INFO_1003).cast::<u8>(),
                Some(&mut parameter_error),
            )
        };
        if password_status == NERR_UserNotFound {
            return Err(AccountUpdateError::NotFound(account.to_owned()));
        }
        if password_status != NERR_Success {
            return Err(AccountUpdateError::Password(api_error(
                "NetUserSetInfo(level 1003)",
                password_status,
                parameter_error,
            )));
        }

        let flags_info = USER_INFO_1008 {
            usri1008_flags: windows::Win32::NetworkManagement::NetManagement::USER_ACCOUNT_FLAGS(
                enabled_flags(current_flags),
            ),
        };
        parameter_error = 0;
        let flags_status = unsafe {
            NetUserSetInfo(
                PCWSTR::null(),
                account_pcwstr,
                1008,
                (&flags_info as *const USER_INFO_1008).cast::<u8>(),
                Some(&mut parameter_error),
            )
        };
        if flags_status != NERR_Success {
            return Err(AccountUpdateError::EnableAfterPasswordCleared(api_error(
                "NetUserSetInfo(level 1008)",
                flags_status,
                parameter_error,
            )));
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use platform::{clear_password_and_enable, list_local_accounts};

#[cfg(not(windows))]
pub fn list_local_accounts() -> Result<Vec<LocalAccount>, AccountUpdateError> {
    Err(AccountUpdateError::Inventory(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn clear_password_and_enable(_account: &str) -> Result<(), AccountUpdateError> {
    Err(AccountUpdateError::Password(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{enabled_flags, validate_account_name, AccountUpdateError};

    #[test]
    fn account_validation_rejects_empty_control_and_oversized_names() {
        assert_eq!(
            validate_account_name("  "),
            Err(AccountUpdateError::InvalidAccount)
        );
        assert_eq!(
            validate_account_name("bad\rname"),
            Err(AccountUpdateError::InvalidAccount)
        );
        assert_eq!(
            validate_account_name(&"a".repeat(257)),
            Err(AccountUpdateError::InvalidAccount)
        );
        assert_eq!(validate_account_name("User One"), Ok("User One"));
    }

    #[test]
    fn enabling_clears_only_the_account_disabled_flag() {
        assert_eq!(enabled_flags(0x0001_0202), 0x0001_0200);
        assert_eq!(enabled_flags(0x0001_0200), 0x0001_0200);
    }
}
