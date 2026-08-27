//! Documented NetAPI boundary for local-account inventory and updates.
//!
//! Account enumeration uses `NetUserEnum` level 1. RID-directed rename inventory uses level 20,
//! whose `usri20_user_id` is the local SAM relative identifier on supported LetRecovery systems
//! (Windows 7+). Rename uses the documented `NetUserSetInfo` level 0 / `USER_INFO_0` contract and
//! reads level 20 back. Password and flag changes use separate levels 1003 and 1008 so unrelated
//! account fields are never reset.

use std::fmt;

#[cfg(not(windows))]
use zeroize::Zeroizing;

const BUILTIN_TRANSITION_LSA_SECRET_NAME: &str = "L$LetRecoveryBuiltinAdministratorPassword";
const MAX_BUILTIN_TRANSITION_PASSWORD_UTF16: usize = 127;
const DEFAULT_OOBE_ACCOUNT_NAME: &str = "defaultuser0";

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
    Rename(String),
    Prepare(String),
    Delete(String),
    Secret(String),
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
            Self::Rename(detail) => write!(formatter, "failed to rename local account: {detail}"),
            Self::Prepare(detail) => write!(
                formatter,
                "failed to prepare the built-in local account: {detail}"
            ),
            Self::Delete(detail) => write!(formatter, "failed to delete local account: {detail}"),
            Self::Secret(detail) => write!(
                formatter,
                "failed to manage the built-in account transition secret: {detail}"
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

/// Windows local user names are limited to 20 UTF-16 code units, may not end in a period, and may
/// not contain the punctuation/control characters documented for `USER_INFO_0`/`USER_INFO_20`.
pub fn validate_new_account_name(account: &str) -> Result<&str, AccountUpdateError> {
    let trimmed = account.trim();
    if trimmed.is_empty()
        || account != trimmed
        || trimmed.encode_utf16().count() > 20
        || trimmed.ends_with('.')
        || trimmed
            .chars()
            .any(|character| character.is_control() || r#"\/,[]:|<>+=;?*""#.contains(character))
    {
        return Err(AccountUpdateError::InvalidAccount);
    }
    Ok(trimmed)
}

/// Encode a validated local-account name as lowercase UTF-16 code units. The resulting argument
/// contains only ASCII hex, so Setup's command-line parser never has to interpret a legal account
/// name containing spaces, ampersands, parentheses or other command metacharacters.
pub fn encode_account_name_utf16_hex(account: &str) -> Result<String, AccountUpdateError> {
    let account = validate_new_account_name(account)?;
    Ok(account
        .encode_utf16()
        .map(|unit| format!("{unit:04x}"))
        .collect())
}

pub fn decode_account_name_utf16_hex(encoded: &str) -> Result<String, AccountUpdateError> {
    if encoded.is_empty()
        || encoded.len() > 80
        // Each UTF-16 code unit is exactly four ASCII hex characters.
        || (encoded.len() & 3) != 0
        || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AccountUpdateError::InvalidAccount);
    }
    let units = encoded
        .as_bytes()
        .chunks_exact(4)
        .map(|chunk| {
            let text =
                std::str::from_utf8(chunk).map_err(|_| AccountUpdateError::InvalidAccount)?;
            u16::from_str_radix(text, 16).map_err(|_| AccountUpdateError::InvalidAccount)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let account = String::from_utf16(&units).map_err(|_| AccountUpdateError::InvalidAccount)?;
    validate_new_account_name(&account)?;
    Ok(account)
}

pub const fn enabled_flags(flags: u32) -> u32 {
    flags & !0x0000_0002
}

fn validate_default_oobe_cleanup_identity(
    account: &LocalAccount,
    current_account: &str,
    sid: &str,
) -> Result<(), AccountUpdateError> {
    let rid = sid
        .rsplit_once('-')
        .and_then(|(_, value)| value.parse::<u32>().ok())
        .ok_or_else(|| AccountUpdateError::Delete("defaultuser0 SID is malformed".to_owned()))?;
    if !account.name.eq_ignore_ascii_case(DEFAULT_OOBE_ACCOUNT_NAME)
        || !account.disabled
        || current_account.eq_ignore_ascii_case(DEFAULT_OOBE_ACCOUNT_NAME)
        || rid < 1000
    {
        return Err(AccountUpdateError::Delete(
            "refusing to delete a non-disabled, current, renamed built-in, or unexpected OOBE account"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_builtin_transition_password(password: &str) -> Result<(), AccountUpdateError> {
    let length = password.encode_utf16().count();
    if length == 0
        || length > MAX_BUILTIN_TRANSITION_PASSWORD_UTF16
        || password
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        return Err(AccountUpdateError::Secret(
            "secret has an invalid password length or character".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::mem;
    use std::ptr;
    use std::slice;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{LocalFree, ERROR_MORE_DATA, HLOCAL};
    use windows::Win32::NetworkManagement::NetManagement::{
        NERR_Success, NERR_UserNotFound, NetApiBufferFree, NetUserDel, NetUserEnum, NetUserGetInfo,
        NetUserSetInfo, FILTER_NORMAL_ACCOUNT, MAX_PREFERRED_LENGTH, UF_ACCOUNTDISABLE,
        USER_INFO_0, USER_INFO_1, USER_INFO_1003, USER_INFO_1008, USER_INFO_20, USER_INFO_4,
    };
    use windows::Win32::Security::Authentication::Identity::{
        LsaClose, LsaFreeMemory, LsaNtStatusToWinError, LsaOpenPolicy, LsaRetrievePrivateData,
        LsaStorePrivateData, LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING,
        POLICY_CREATE_SECRET, POLICY_GET_PRIVATE_INFORMATION,
    };
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::System::WindowsProgramming::GetUserNameW;
    use windows::Win32::UI::Shell::DeleteProfileW;
    use zeroize::Zeroizing;

    use super::{
        enabled_flags, validate_account_name, validate_builtin_transition_password,
        validate_default_oobe_cleanup_identity, validate_new_account_name, AccountUpdateError,
        LocalAccount, BUILTIN_TRANSITION_LSA_SECRET_NAME, DEFAULT_OOBE_ACCOUNT_NAME,
    };

    const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034_u32 as i32;

    struct PolicyHandle(LSA_HANDLE);

    impl Drop for PolicyHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = LsaClose(self.0);
                }
            }
        }
    }

    struct LsaAllocatedString(*mut LSA_UNICODE_STRING);

    impl Drop for LsaAllocatedString {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LsaFreeMemory(Some(self.0.cast::<c_void>()));
                }
            }
        }
    }

    fn lsa_error(
        operation: &str,
        status: windows::Win32::Foundation::NTSTATUS,
    ) -> AccountUpdateError {
        let win32 = unsafe { LsaNtStatusToWinError(status) };
        AccountUpdateError::Secret(format!(
            "{operation} returned NTSTATUS 0x{:08x} (Win32 {win32})",
            status.0 as u32
        ))
    }

    fn lsa_unicode_string(
        value: &mut Zeroizing<Vec<u16>>,
    ) -> Result<LSA_UNICODE_STRING, AccountUpdateError> {
        let content_units = value
            .len()
            .checked_sub(1)
            .ok_or_else(|| AccountUpdateError::Secret("LSA string has no terminator".to_owned()))?;
        let length_bytes = content_units
            .checked_mul(mem::size_of::<u16>())
            .ok_or_else(|| AccountUpdateError::Secret("LSA string length overflow".to_owned()))?;
        let maximum_bytes = value
            .len()
            .checked_mul(mem::size_of::<u16>())
            .ok_or_else(|| AccountUpdateError::Secret("LSA string capacity overflow".to_owned()))?;
        Ok(LSA_UNICODE_STRING {
            Length: u16::try_from(length_bytes).map_err(|_| {
                AccountUpdateError::Secret("LSA string exceeds USHORT length".to_owned())
            })?,
            MaximumLength: u16::try_from(maximum_bytes).map_err(|_| {
                AccountUpdateError::Secret("LSA string exceeds USHORT capacity".to_owned())
            })?,
            Buffer: PWSTR(value.as_mut_ptr()),
        })
    }

    fn open_policy(access: u32) -> Result<PolicyHandle, AccountUpdateError> {
        // Microsoft documents every LSA_OBJECT_ATTRIBUTES member as unused for LsaOpenPolicy;
        // initialize the complete structure to zero and identify the local machine with NULL.
        let attributes = LSA_OBJECT_ATTRIBUTES::default();
        let mut handle = LSA_HANDLE::default();
        let status = unsafe { LsaOpenPolicy(None, &attributes, access, &mut handle) };
        if status.0 != 0 {
            return Err(lsa_error("LsaOpenPolicy", status));
        }
        if handle.is_invalid() {
            return Err(AccountUpdateError::Secret(
                "LsaOpenPolicy succeeded with an invalid handle".to_owned(),
            ));
        }
        Ok(PolicyHandle(handle))
    }

    /// Store the one credential that must cross the OOBE account switch. Microsoft documents
    /// L$ secrets as local-only and LsaStorePrivateData as encrypting the value before storage.
    pub fn store_builtin_transition_password(password: &str) -> Result<(), AccountUpdateError> {
        validate_builtin_transition_password(password)?;
        let handle = open_policy(POLICY_CREATE_SECRET as u32)?;
        let mut key = Zeroizing::new(
            BUILTIN_TRANSITION_LSA_SECRET_NAME
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>(),
        );
        let mut secret = Zeroizing::new(password.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
        let key = lsa_unicode_string(&mut key)?;
        let secret = lsa_unicode_string(&mut secret)?;
        let status = unsafe { LsaStorePrivateData(handle.0, &key, Some(&secret)) };
        if status.0 != 0 {
            return Err(lsa_error("LsaStorePrivateData", status));
        }
        Ok(())
    }

    pub fn retrieve_builtin_transition_password_optional(
    ) -> Result<Option<Zeroizing<String>>, AccountUpdateError> {
        let handle = open_policy(POLICY_GET_PRIVATE_INFORMATION as u32)?;
        let mut key = Zeroizing::new(
            BUILTIN_TRANSITION_LSA_SECRET_NAME
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>(),
        );
        let key = lsa_unicode_string(&mut key)?;
        let mut raw = ptr::null_mut::<LSA_UNICODE_STRING>();
        let status = unsafe { LsaRetrievePrivateData(handle.0, &key, &mut raw) };
        if status.0 == STATUS_OBJECT_NAME_NOT_FOUND {
            return Ok(None);
        }
        if status.0 != 0 {
            return Err(lsa_error("LsaRetrievePrivateData", status));
        }
        let allocation = LsaAllocatedString(raw);
        if allocation.0.is_null() {
            return Err(AccountUpdateError::Secret(
                "LsaRetrievePrivateData succeeded with a null result".to_owned(),
            ));
        }
        let returned = unsafe { &*allocation.0 };
        if returned.Buffer.is_null()
            || returned.Length == 0
            || (returned.Length & 1) != 0
            || returned.Length > returned.MaximumLength
        {
            return Err(AccountUpdateError::Secret(
                "LsaRetrievePrivateData returned an invalid LSA_UNICODE_STRING".to_owned(),
            ));
        }
        let units = usize::from(returned.Length) / mem::size_of::<u16>();
        let password = Zeroizing::new(
            String::from_utf16(unsafe { slice::from_raw_parts(returned.Buffer.0, units) })
                .map_err(|_| {
                    AccountUpdateError::Secret("LSA secret is not valid UTF-16".to_owned())
                })?,
        );
        validate_builtin_transition_password(password.as_str())?;
        Ok(Some(password))
    }

    pub fn retrieve_builtin_transition_password() -> Result<Zeroizing<String>, AccountUpdateError> {
        retrieve_builtin_transition_password_optional()?
            .ok_or_else(|| AccountUpdateError::Secret("the local LSA secret is missing".to_owned()))
    }

    pub fn delete_builtin_transition_password() -> Result<(), AccountUpdateError> {
        let handle = open_policy(POLICY_CREATE_SECRET as u32)?;
        let mut key = Zeroizing::new(
            BUILTIN_TRANSITION_LSA_SECRET_NAME
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>(),
        );
        let key = lsa_unicode_string(&mut key)?;
        let status = unsafe { LsaStorePrivateData(handle.0, &key, None) };
        if status.0 != 0 && status.0 != STATUS_OBJECT_NAME_NOT_FOUND {
            return Err(lsa_error("LsaStorePrivateData(delete)", status));
        }
        if retrieve_builtin_transition_password_optional()?.is_some() {
            return Err(AccountUpdateError::Secret(
                "LSA secret survived deletion".to_owned(),
            ));
        }
        Ok(())
    }

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

    /// Return the account name attached to the current process token. `GetUserNameW` reports the
    /// required buffer length in UTF-16 code units including the terminator; the local-account
    /// transition uses this only to prove that OOBE has moved from its temporary account to the
    /// requested RID-500 account before deleting the temporary profile.
    pub fn current_local_account_name() -> Result<String, AccountUpdateError> {
        let mut buffer = vec![0_u16; 32_768];
        let mut length = u32::try_from(buffer.len()).map_err(|_| {
            AccountUpdateError::Inventory("GetUserNameW buffer length overflow".to_owned())
        })?;
        unsafe { GetUserNameW(PWSTR(buffer.as_mut_ptr()), &mut length) }
            .map_err(|error| AccountUpdateError::Inventory(format!("GetUserNameW: {error}")))?;
        let length = usize::try_from(length).map_err(|_| {
            AccountUpdateError::Inventory("GetUserNameW result length overflow".to_owned())
        })?;
        if length == 0 || length > buffer.len() || buffer[length - 1] != 0 {
            return Err(AccountUpdateError::Inventory(
                "GetUserNameW returned an invalid terminated length".to_owned(),
            ));
        }
        String::from_utf16(&buffer[..length - 1]).map_err(|_| {
            AccountUpdateError::Inventory("GetUserNameW returned invalid UTF-16".to_owned())
        })
    }

    /// Resolve a local account to its canonical string SID through level-4 `NetUserGetInfo`.
    /// Microsoft documents `usri4_user_sid` as the account SID and requires the string allocated
    /// by `ConvertSidToStringSidW` to be released with `LocalFree`.
    pub fn local_account_sid_string(account: &str) -> Result<String, AccountUpdateError> {
        let account = validate_new_account_name(account)?;
        let account_wide: Vec<u16> = account.encode_utf16().chain(Some(0)).collect();
        let mut raw = ptr::null_mut::<u8>();
        let status =
            unsafe { NetUserGetInfo(PCWSTR::null(), PCWSTR(account_wide.as_ptr()), 4, &mut raw) };
        let buffer = NetBuffer(raw);
        if status == NERR_UserNotFound {
            return Err(AccountUpdateError::NotFound(account.to_owned()));
        }
        if status != NERR_Success {
            return Err(AccountUpdateError::Inventory(api_error(
                "NetUserGetInfo(level 4)",
                status,
                0,
            )));
        }
        if buffer.0.is_null() {
            return Err(AccountUpdateError::Inventory(
                "NetUserGetInfo(level 4) succeeded with a null buffer".to_owned(),
            ));
        }
        let info = unsafe { &*buffer.0.cast::<USER_INFO_4>() };
        if info.usri4_user_sid.is_invalid() {
            return Err(AccountUpdateError::Inventory(
                "NetUserGetInfo(level 4) returned an invalid account SID".to_owned(),
            ));
        }
        let mut string_sid = PWSTR::null();
        unsafe { ConvertSidToStringSidW(info.usri4_user_sid, &mut string_sid) }.map_err(
            |error| AccountUpdateError::Inventory(format!("ConvertSidToStringSidW: {error}")),
        )?;
        struct LocalString(PWSTR);
        impl Drop for LocalString {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe {
                        let _ = LocalFree(HLOCAL(self.0 .0.cast::<c_void>()));
                    }
                }
            }
        }
        let string_sid = LocalString(string_sid);
        unsafe { string_sid.0.to_string() }.map_err(|error| {
            AccountUpdateError::Inventory(format!("account SID string is invalid: {error}"))
        })
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

    fn account_name_for_rid(rid: u32) -> Result<String, AccountUpdateError> {
        let mut resume = 0_u32;
        loop {
            let mut raw = ptr::null_mut::<u8>();
            let mut entries_read = 0_u32;
            let mut total_entries = 0_u32;
            let status = unsafe {
                NetUserEnum(
                    PCWSTR::null(),
                    20,
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
                    "NetUserEnum(level 20)",
                    status,
                    0,
                )));
            }
            if entries_read != 0 {
                if buffer.0.is_null() {
                    return Err(AccountUpdateError::Inventory(
                        "NetUserEnum(level 20) returned entries with a null buffer".to_owned(),
                    ));
                }
                let entries = unsafe {
                    slice::from_raw_parts(buffer.0.cast::<USER_INFO_20>(), entries_read as usize)
                };
                for entry in entries {
                    if entry.usri20_user_id == rid {
                        return unsafe { pwstr_to_string(entry.usri20_name) };
                    }
                }
            }
            if status == NERR_Success {
                break;
            }
        }
        Err(AccountUpdateError::NotFound(format!("RID {rid}")))
    }

    /// Rename and enable one local SAM account selected by RID, then read both facts back.
    /// `NetUserSetInfo` level 0 is the only level documented to apply `usri0_name`; level 1008
    /// applies only `usri1008_flags`. Setup's specialize pass can disable RID 500 independently,
    /// so a successful rename alone is not sufficient for a later AutoLogon.
    pub fn prepare_local_account_by_rid(
        rid: u32,
        new_name: &str,
    ) -> Result<(), AccountUpdateError> {
        let new_name = validate_new_account_name(new_name)?;
        let current_name = account_name_for_rid(rid)?;
        if !current_name.eq_ignore_ascii_case(new_name) {
            let current_wide: Vec<u16> = current_name.encode_utf16().chain(Some(0)).collect();
            let mut new_wide: Vec<u16> = new_name.encode_utf16().chain(Some(0)).collect();
            let info = USER_INFO_0 {
                usri0_name: PWSTR(new_wide.as_mut_ptr()),
            };
            let mut parameter_error = 0_u32;
            let status = unsafe {
                NetUserSetInfo(
                    PCWSTR::null(),
                    PCWSTR(current_wide.as_ptr()),
                    0,
                    (&info as *const USER_INFO_0).cast::<u8>(),
                    Some(&mut parameter_error),
                )
            };
            if status == NERR_UserNotFound {
                return Err(AccountUpdateError::NotFound(current_name));
            }
            if status != NERR_Success {
                return Err(AccountUpdateError::Rename(api_error(
                    "NetUserSetInfo(level 0)",
                    status,
                    parameter_error,
                )));
            }
        }
        let actual = account_name_for_rid(rid)?;
        if actual != new_name {
            return Err(AccountUpdateError::Prepare(
                "RID name readback did not match exactly after NetUserSetInfo(level 0)".to_owned(),
            ));
        }
        let actual_wide: Vec<u16> = actual.encode_utf16().chain(Some(0)).collect();
        let account = PCWSTR(actual_wide.as_ptr());
        let current_flags = unsafe { get_account_flags(account)? };
        let desired_flags = enabled_flags(current_flags);
        if desired_flags != current_flags {
            let flags_info = USER_INFO_1008 {
                usri1008_flags:
                    windows::Win32::NetworkManagement::NetManagement::USER_ACCOUNT_FLAGS(
                        desired_flags,
                    ),
            };
            let mut parameter_error = 0_u32;
            let status = unsafe {
                NetUserSetInfo(
                    PCWSTR::null(),
                    account,
                    1008,
                    (&flags_info as *const USER_INFO_1008).cast::<u8>(),
                    Some(&mut parameter_error),
                )
            };
            if status != NERR_Success {
                return Err(AccountUpdateError::Prepare(api_error(
                    "NetUserSetInfo(level 1008)",
                    status,
                    parameter_error,
                )));
            }
        }
        let final_name = account_name_for_rid(rid)?;
        let final_flags = unsafe { get_account_flags(account)? };
        if final_name != new_name || final_flags & UF_ACCOUNTDISABLE.0 != 0 {
            return Err(AccountUpdateError::Prepare(
                "RID name or enabled-state readback did not match after preparation".to_owned(),
            ));
        }
        Ok(())
    }

    fn local_account_exists(account: PCWSTR) -> Result<bool, AccountUpdateError> {
        let mut raw = ptr::null_mut::<u8>();
        let status = unsafe { NetUserGetInfo(PCWSTR::null(), account, 0, &mut raw) };
        let buffer = NetBuffer(raw);
        if status == NERR_UserNotFound {
            return Ok(false);
        }
        if status != NERR_Success {
            return Err(AccountUpdateError::Inventory(api_error(
                "NetUserGetInfo(level 0)",
                status,
                0,
            )));
        }
        if buffer.0.is_null() {
            return Err(AccountUpdateError::Inventory(
                "NetUserGetInfo(level 0) succeeded with a null buffer".to_owned(),
            ));
        }
        Ok(true)
    }

    /// Delete one exact local account from the current computer, then prove through level-0
    /// `NetUserGetInfo` that the account no longer exists. `servername = NULL` is the documented
    /// local-computer binding for both APIs; `NetUserDel` has been available since Windows 2000.
    pub fn delete_local_account(account: &str) -> Result<(), AccountUpdateError> {
        let account = validate_new_account_name(account)?;
        let account_wide: Vec<u16> = account.encode_utf16().chain(Some(0)).collect();
        let account_pcwstr = PCWSTR(account_wide.as_ptr());
        if !local_account_exists(account_pcwstr)? {
            return Ok(());
        }
        let status = unsafe { NetUserDel(PCWSTR::null(), account_pcwstr) };
        if status != NERR_Success && status != NERR_UserNotFound {
            return Err(AccountUpdateError::Delete(api_error(
                "NetUserDel",
                status,
                0,
            )));
        }
        match local_account_exists(account_pcwstr) {
            Ok(false) => Ok(()),
            Ok(true) => Err(AccountUpdateError::Delete(
                "NetUserDel returned success but the account still exists".to_owned(),
            )),
            Err(error) => Err(AccountUpdateError::Delete(format!(
                "post-delete readback failed: {error}"
            ))),
        }
    }

    /// Delete one exact temporary account and its profile. The caller supplies the SID captured
    /// before deletion so a retry can still call `DeleteProfileW` after `NetUserDel` has already
    /// committed. Since Windows Vista, both optional `DeleteProfileW` parameters must be NULL.
    pub fn delete_local_account_and_profile(
        account: &str,
        expected_sid: &str,
    ) -> Result<(), AccountUpdateError> {
        let account = validate_new_account_name(account)?;
        if expected_sid.is_empty()
            || expected_sid.len() > 184
            || !expected_sid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
            || !expected_sid.starts_with("S-1-")
        {
            return Err(AccountUpdateError::InvalidAccount);
        }
        match local_account_sid_string(account) {
            Ok(actual) if actual != expected_sid => {
                return Err(AccountUpdateError::Delete(
                    "temporary account SID changed before deletion".to_owned(),
                ));
            }
            Ok(_) => delete_local_account(account)?,
            Err(AccountUpdateError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }

        let profile_key = format!(
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{expected_sid}"
        );
        if crate::registry::OfflineRegistry::key_exists(&profile_key)
            .map_err(|error| AccountUpdateError::Delete(error.to_string()))?
        {
            let sid_wide: Vec<u16> = expected_sid.encode_utf16().chain(Some(0)).collect();
            unsafe { DeleteProfileW(PCWSTR(sid_wide.as_ptr()), PCWSTR::null(), PCWSTR::null()) }
                .map_err(|error| AccountUpdateError::Delete(format!("DeleteProfileW: {error}")))?;
        }
        if crate::registry::OfflineRegistry::key_exists(&profile_key)
            .map_err(|error| AccountUpdateError::Delete(error.to_string()))?
        {
            return Err(AccountUpdateError::Delete(
                "temporary profile registry key survived DeleteProfileW".to_owned(),
            ));
        }
        Ok(())
    }

    /// Remove only Windows Setup's exact disabled `defaultuser0` residue. The candidate must be a
    /// non-current RID 1000+ account, and its SID is captured before `NetUserDel` so the documented
    /// `DeleteProfileW(SID, NULL, NULL)` cleanup remains retryable after account deletion.
    pub fn cleanup_disabled_default_oobe_account() -> Result<bool, AccountUpdateError> {
        let matches = list_local_accounts()?
            .into_iter()
            .filter(|account| account.name.eq_ignore_ascii_case(DEFAULT_OOBE_ACCOUNT_NAME))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Ok(false);
        }
        if matches.len() != 1 {
            return Err(AccountUpdateError::Delete(
                "defaultuser0 account inventory is ambiguous".to_owned(),
            ));
        }
        let account = &matches[0];
        let current_account = current_local_account_name()?;
        let sid = local_account_sid_string(&account.name)?;
        validate_default_oobe_cleanup_identity(account, &current_account, &sid)?;
        delete_local_account_and_profile(&account.name, &sid)?;
        if list_local_accounts()?.iter().any(|candidate| {
            candidate
                .name
                .eq_ignore_ascii_case(DEFAULT_OOBE_ACCOUNT_NAME)
        }) {
            return Err(AccountUpdateError::Delete(
                "defaultuser0 survived account/profile cleanup".to_owned(),
            ));
        }
        Ok(true)
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
pub use platform::{
    cleanup_disabled_default_oobe_account, clear_password_and_enable, current_local_account_name,
    delete_builtin_transition_password, delete_local_account, delete_local_account_and_profile,
    list_local_accounts, local_account_sid_string, prepare_local_account_by_rid,
    retrieve_builtin_transition_password, retrieve_builtin_transition_password_optional,
    store_builtin_transition_password,
};

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

#[cfg(not(windows))]
pub fn prepare_local_account_by_rid(_rid: u32, _new_name: &str) -> Result<(), AccountUpdateError> {
    Err(AccountUpdateError::Prepare(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn delete_local_account(_account: &str) -> Result<(), AccountUpdateError> {
    Err(AccountUpdateError::Delete(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn current_local_account_name() -> Result<String, AccountUpdateError> {
    Err(AccountUpdateError::Inventory(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn local_account_sid_string(_account: &str) -> Result<String, AccountUpdateError> {
    Err(AccountUpdateError::Inventory(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn delete_local_account_and_profile(
    _account: &str,
    _expected_sid: &str,
) -> Result<(), AccountUpdateError> {
    Err(AccountUpdateError::Delete(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn cleanup_disabled_default_oobe_account() -> Result<bool, AccountUpdateError> {
    Err(AccountUpdateError::Delete(
        "local account APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn store_builtin_transition_password(_password: &str) -> Result<(), AccountUpdateError> {
    Err(AccountUpdateError::Secret(
        "LSA private-data APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn retrieve_builtin_transition_password() -> Result<Zeroizing<String>, AccountUpdateError> {
    Err(AccountUpdateError::Secret(
        "LSA private-data APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn retrieve_builtin_transition_password_optional(
) -> Result<Option<Zeroizing<String>>, AccountUpdateError> {
    Err(AccountUpdateError::Secret(
        "LSA private-data APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn delete_builtin_transition_password() -> Result<(), AccountUpdateError> {
    Err(AccountUpdateError::Secret(
        "LSA private-data APIs are available only on Windows".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_account_name_utf16_hex, enabled_flags, encode_account_name_utf16_hex,
        validate_account_name, validate_builtin_transition_password,
        validate_default_oobe_cleanup_identity, validate_new_account_name, AccountUpdateError,
        LocalAccount,
    };

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

    #[test]
    fn rename_validation_matches_documented_local_user_name_contract() {
        assert_eq!(validate_new_account_name("Ops Admin"), Ok("Ops Admin"));
        for invalid in [
            "",
            " trailing",
            "trailing ",
            "trailing.",
            "bad/name",
            "bad,name",
        ] {
            assert_eq!(
                validate_new_account_name(invalid),
                Err(AccountUpdateError::InvalidAccount),
                "{invalid:?}"
            );
        }
        assert_eq!(
            validate_new_account_name(&"a".repeat(21)),
            Err(AccountUpdateError::InvalidAccount)
        );
        let encoded = encode_account_name_utf16_hex("O'Brien & Ops").unwrap();
        assert!(encoded.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            decode_account_name_utf16_hex(&encoded).unwrap(),
            "O'Brien & Ops"
        );
        assert!(decode_account_name_utf16_hex("d800").is_err());
    }

    #[test]
    fn transition_secret_accepts_the_unattend_password_contract_only() {
        assert!(validate_builtin_transition_password("Secret!").is_ok());
        assert!(validate_builtin_transition_password("").is_err());
        assert!(validate_builtin_transition_password("bad\nsecret").is_err());
        assert!(validate_builtin_transition_password(&"x".repeat(127)).is_ok());
        assert!(validate_builtin_transition_password(&"x".repeat(128)).is_err());
    }

    #[test]
    fn default_oobe_cleanup_requires_exact_disabled_noncurrent_rid_1000_plus_identity() {
        let candidate = LocalAccount {
            name: "defaultuser0".to_owned(),
            disabled: true,
        };
        assert!(validate_default_oobe_cleanup_identity(
            &candidate,
            "LRAdmin11",
            "S-1-5-21-100-200-300-1001"
        )
        .is_ok());
        for (account, current, sid) in [
            (
                LocalAccount {
                    name: "defaultuser0".to_owned(),
                    disabled: false,
                },
                "LRAdmin11",
                "S-1-5-21-100-200-300-1001",
            ),
            (
                candidate.clone(),
                "defaultuser0",
                "S-1-5-21-100-200-300-1001",
            ),
            (candidate.clone(), "LRAdmin11", "S-1-5-21-100-200-300-500"),
            (candidate.clone(), "LRAdmin11", "malformed"),
        ] {
            assert!(validate_default_oobe_cleanup_identity(&account, current, sid).is_err());
        }
    }
}
