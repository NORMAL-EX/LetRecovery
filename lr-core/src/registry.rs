//! Shared offline-registry boundary.
//!
//! Queries, hive loading, value updates, enumeration and deletion use the
//! documented Win32 registry APIs directly. Importing a complete `.reg` file
//! remains an explicit `reg.exe import` compatibility boundary because Windows
//! does not expose a public API that parses the `.reg` interchange format.

use anyhow::Result;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

pub struct OfflineRegistry;

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA,
        ERROR_NOT_ALL_ASSIGNED, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, HANDLE, LUID, WIN32_ERROR,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW, RegGetValueW,
        RegLoadKeyW, RegOpenKeyExW, RegSetValueExW, RegUnLoadKeyW, HKEY, HKEY_CLASSES_ROOT,
        HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
        KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, KEY_WRITE, REG_BINARY,
        REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_ROUTINE_FLAGS, REG_SAM_FLAGS,
        REG_SZ, REG_VALUE_TYPE, RRF_NOEXPAND, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RRF_ZEROONFAILURE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const SE_BACKUP_NAME: &str = "SeBackupPrivilege";
    const SE_RESTORE_NAME: &str = "SeRestorePrivilege";
    const MAX_REGISTRY_NAME_CHARS: usize = 32_767;

    struct OwnedKey(HKEY);

    impl Drop for OwnedKey {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    struct OwnedToken(HANDLE);

    impl Drop for OwnedToken {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn api_error(operation: &str, status: WIN32_ERROR) -> anyhow::Error {
        anyhow::anyhow!(
            "{operation} failed with Win32 error {}: {}",
            status.0,
            std::io::Error::from_raw_os_error(status.0 as i32)
        )
    }

    fn ensure_success(operation: &str, status: WIN32_ERROR) -> Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(api_error(operation, status))
        }
    }

    fn split_key_path(key_path: &str) -> Result<(HKEY, &str)> {
        let key_path = key_path.trim();
        if key_path.is_empty() || key_path.contains('\0') {
            bail!("registry key path is empty or contains NUL");
        }
        let (root_name, subkey) = key_path
            .split_once('\\')
            .map_or((key_path, ""), |(root, subkey)| (root, subkey));
        let root = match root_name.to_ascii_uppercase().as_str() {
            "HKLM" | "HKEY_LOCAL_MACHINE" => HKEY_LOCAL_MACHINE,
            "HKCU" | "HKEY_CURRENT_USER" => HKEY_CURRENT_USER,
            "HKU" | "HKEY_USERS" => HKEY_USERS,
            "HKCR" | "HKEY_CLASSES_ROOT" => HKEY_CLASSES_ROOT,
            "HKCC" | "HKEY_CURRENT_CONFIG" => HKEY_CURRENT_CONFIG,
            _ => bail!("unsupported registry root in path: {key_path}"),
        };
        Ok((root, subkey.trim_end_matches('\\')))
    }

    fn open_key(key_path: &str, access: REG_SAM_FLAGS) -> Result<OwnedKey> {
        let (root, subkey) = split_key_path(key_path)?;
        if subkey.is_empty() {
            bail!("opening a predefined registry root is not supported: {key_path}");
        }
        let subkey = wide(subkey);
        let mut handle = HKEY::default();
        let status =
            unsafe { RegOpenKeyExW(root, PCWSTR(subkey.as_ptr()), 0, access, &mut handle) };
        ensure_success(&format!("RegOpenKeyExW({key_path})"), status)?;
        Ok(OwnedKey(handle))
    }

    fn query_value_bytes_optional(
        key_path: &str,
        value_name: &str,
        flags: REG_ROUTINE_FLAGS,
    ) -> Result<Option<(REG_VALUE_TYPE, Vec<u8>)>> {
        if value_name.contains('\0') {
            bail!("registry value name contains NUL");
        }
        let (root, subkey) = split_key_path(key_path)?;
        let subkey = wide(subkey);
        let value_name = wide(value_name);

        for _ in 0..3 {
            let mut value_type = REG_VALUE_TYPE::default();
            let mut size = 0_u32;
            let status = unsafe {
                RegGetValueW(
                    root,
                    PCWSTR(subkey.as_ptr()),
                    PCWSTR(value_name.as_ptr()),
                    flags | RRF_ZEROONFAILURE,
                    Some(&mut value_type),
                    None,
                    Some(&mut size),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
                return Err(api_error(
                    &format!("RegGetValueW(size for {key_path})"),
                    status,
                ));
            }

            let mut data = vec![0_u8; size as usize];
            let mut actual_size = size;
            let data_pointer = if data.is_empty() {
                None
            } else {
                Some(data.as_mut_ptr().cast::<c_void>())
            };
            let status = unsafe {
                RegGetValueW(
                    root,
                    PCWSTR(subkey.as_ptr()),
                    PCWSTR(value_name.as_ptr()),
                    flags | RRF_ZEROONFAILURE,
                    Some(&mut value_type),
                    data_pointer,
                    Some(&mut actual_size),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status == ERROR_MORE_DATA {
                continue;
            }
            ensure_success(&format!("RegGetValueW({key_path})"), status)?;
            data.truncate(actual_size as usize);
            return Ok(Some((value_type, data)));
        }
        bail!("registry value changed repeatedly while reading {key_path}");
    }

    fn decode_registry_string(data: &[u8], key_path: &str, value_name: &str) -> Result<String> {
        if !data.len().is_multiple_of(2) {
            bail!("registry string has an odd byte length: {key_path}\\{value_name}");
        }
        let mut utf16 = Vec::with_capacity(data.len() / 2);
        for chunk in data.chunks_exact(2) {
            utf16.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        while utf16.last() == Some(&0) {
            utf16.pop();
        }
        if utf16.contains(&0) {
            bail!("registry string contains an embedded NUL: {key_path}\\{value_name}");
        }
        String::from_utf16(&utf16)
            .with_context(|| format!("registry string is invalid UTF-16: {key_path}\\{value_name}"))
    }

    pub fn query_string_optional(key_path: &str, value_name: &str) -> Result<Option<String>> {
        query_value_bytes_optional(
            key_path,
            value_name,
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
        )?
        .map(|(_, data)| decode_registry_string(&data, key_path, value_name))
        .transpose()
    }

    pub fn query_string(key_path: &str, value_name: &str) -> Result<String> {
        query_string_optional(key_path, value_name)?.ok_or_else(|| {
            anyhow::anyhow!("registry string value does not exist: {key_path}\\{value_name}")
        })
    }

    pub fn query_dword(key_path: &str, value_name: &str) -> Result<u32> {
        let (_, data) = query_value_bytes_optional(key_path, value_name, RRF_RT_REG_DWORD)?
            .ok_or_else(|| {
                anyhow::anyhow!("registry DWORD value does not exist: {key_path}\\{value_name}")
            })?;
        let bytes: [u8; 4] = data.try_into().map_err(|_| {
            anyhow::anyhow!("registry DWORD has an invalid size: {key_path}\\{value_name}")
        })?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn query_binary(key_path: &str, value_name: &str) -> Result<Vec<u8>> {
        query_value_bytes_optional(key_path, value_name, RRF_RT_REG_BINARY)?
            .map(|(_, data)| data)
            .ok_or_else(|| {
                anyhow::anyhow!("registry binary value does not exist: {key_path}\\{value_name}")
            })
    }

    pub fn key_exists(key_path: &str) -> Result<bool> {
        let (root, subkey) = split_key_path(key_path)?;
        if subkey.is_empty() {
            return Ok(true);
        }
        let subkey = wide(subkey);
        let mut handle = HKEY::default();
        let status =
            unsafe { RegOpenKeyExW(root, PCWSTR(subkey.as_ptr()), 0, KEY_READ, &mut handle) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        ensure_success(&format!("RegOpenKeyExW({key_path})"), status)?;
        drop(OwnedKey(handle));
        Ok(true)
    }

    pub fn enumerate_subkeys(key_path: &str) -> Result<Vec<String>> {
        let key = open_key(key_path, KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE)?;
        let mut names = Vec::new();
        let mut index = 0_u32;

        loop {
            let mut capacity = 256_usize;
            loop {
                let mut buffer = vec![0_u16; capacity + 1];
                let mut length = capacity as u32;
                let status = unsafe {
                    RegEnumKeyExW(
                        key.0,
                        index,
                        PWSTR(buffer.as_mut_ptr()),
                        &mut length,
                        None,
                        PWSTR::null(),
                        None,
                        None,
                    )
                };
                if status == ERROR_NO_MORE_ITEMS {
                    return Ok(names);
                }
                if status == ERROR_MORE_DATA {
                    capacity = capacity.saturating_mul(2);
                    if capacity > MAX_REGISTRY_NAME_CHARS {
                        bail!("registry subkey name exceeds the documented maximum");
                    }
                    continue;
                }
                ensure_success(&format!("RegEnumKeyExW({key_path})"), status)?;
                names.push(
                    String::from_utf16(&buffer[..length as usize]).with_context(|| {
                        format!("registry subkey name is invalid UTF-16 below {key_path}")
                    })?,
                );
                index = index
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("registry subkey index overflow"))?;
                break;
            }
        }
    }

    pub fn query_string_values_recursive(key_path: &str, value_name: &str) -> Result<Vec<String>> {
        if !key_exists(key_path)? {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        let mut pending = vec![key_path.to_owned()];
        while let Some(current) = pending.pop() {
            if let Some(value) = query_string_optional(&current, value_name)? {
                values.push(value);
            }
            for child in enumerate_subkeys(&current)? {
                pending.push(format!("{current}\\{child}"));
            }
        }
        Ok(values)
    }

    fn enable_privilege(privilege_name: &str) -> Result<()> {
        let mut raw_token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut raw_token,
            )
            .with_context(|| format!("OpenProcessToken for {privilege_name}"))?;
        }
        let token = OwnedToken(raw_token);
        let name = wide(privilege_name);
        let mut luid = LUID::default();
        unsafe {
            LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(name.as_ptr()), &mut luid)
                .with_context(|| format!("LookupPrivilegeValueW({privilege_name})"))?;
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
                .with_context(|| format!("AdjustTokenPrivileges({privilege_name})"))?;
            let status = GetLastError();
            if status == ERROR_NOT_ALL_ASSIGNED {
                bail!("the process token does not contain {privilege_name}");
            }
            if status != ERROR_SUCCESS {
                return Err(api_error(
                    &format!("AdjustTokenPrivileges({privilege_name})"),
                    status,
                ));
            }
        }
        Ok(())
    }

    fn validate_hive_name(hive_name: &str) -> Result<&str> {
        let hive_name = hive_name.trim();
        if hive_name.is_empty()
            || hive_name.contains(['\\', '/', '\0'])
            || hive_name.chars().any(char::is_control)
        {
            bail!("invalid offline registry hive name");
        }
        Ok(hive_name)
    }

    pub fn load_hive(hive_name: &str, hive_file: &str) -> Result<()> {
        let hive_name = validate_hive_name(hive_name)?;
        let hive_path = Path::new(hive_file);
        let metadata = hive_path
            .symlink_metadata()
            .with_context(|| format!("read registry hive metadata: {}", hive_path.display()))?;
        if !metadata.is_file() {
            bail!(
                "registry hive is not a regular file: {}",
                hive_path.display()
            );
        }
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0
            != 0
        {
            bail!(
                "registry hive must not be a reparse point: {}",
                hive_path.display()
            );
        }
        let absolute = hive_path
            .canonicalize()
            .with_context(|| format!("resolve registry hive path: {}", hive_path.display()))?;
        enable_privilege(SE_RESTORE_NAME)?;
        enable_privilege(SE_BACKUP_NAME)?;
        let hive_name_wide = wide(hive_name);
        let file_wide: Vec<u16> = absolute.as_os_str().encode_wide().chain(Some(0)).collect();
        let status = unsafe {
            RegLoadKeyW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(hive_name_wide.as_ptr()),
                PCWSTR(file_wide.as_ptr()),
            )
        };
        ensure_success(&format!("RegLoadKeyW({hive_name})"), status)?;
        log::info!(
            "已通过 RegLoadKeyW 加载离线注册表配置单元 [{}] <- {}",
            hive_name,
            absolute.display()
        );
        Ok(())
    }

    pub fn unload_hive(hive_name: &str) -> Result<()> {
        let hive_name = validate_hive_name(hive_name)?;
        enable_privilege(SE_RESTORE_NAME)?;
        enable_privilege(SE_BACKUP_NAME)?;
        let hive_name_wide = wide(hive_name);
        let mut last_status = ERROR_SUCCESS;
        for attempt in 0..4 {
            let status =
                unsafe { RegUnLoadKeyW(HKEY_LOCAL_MACHINE, PCWSTR(hive_name_wide.as_ptr())) };
            if status == ERROR_SUCCESS {
                return Ok(());
            }
            last_status = status;
            if attempt != 3 {
                thread::sleep(Duration::from_millis(500));
            }
        }
        Err(api_error(
            &format!("RegUnLoadKeyW({hive_name})"),
            last_status,
        ))
    }

    fn set_value(
        key_path: &str,
        value_name: &str,
        value_type: REG_VALUE_TYPE,
        data: &[u8],
    ) -> Result<()> {
        create_key(key_path)?;
        let key = open_key(key_path, KEY_SET_VALUE)?;
        if value_name.contains('\0') {
            bail!("registry value name contains NUL");
        }
        let value_name = wide(value_name);
        let status = unsafe {
            RegSetValueExW(
                key.0,
                PCWSTR(value_name.as_ptr()),
                0,
                value_type,
                Some(data),
            )
        };
        ensure_success(&format!("RegSetValueExW({key_path})"), status)
    }

    fn utf16_bytes(value: &str) -> Vec<u8> {
        value
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    pub fn set_dword(key_path: &str, value_name: &str, data: u32) -> Result<()> {
        set_value(key_path, value_name, REG_DWORD, &data.to_le_bytes())
    }

    pub fn set_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        set_value(key_path, value_name, REG_SZ, &utf16_bytes(data))
    }

    pub fn set_expand_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        set_value(key_path, value_name, REG_EXPAND_SZ, &utf16_bytes(data))
    }

    pub fn set_binary(key_path: &str, value_name: &str, data: &[u8]) -> Result<()> {
        set_value(key_path, value_name, REG_BINARY, data)
    }

    pub fn create_key(key_path: &str) -> Result<()> {
        let (root, subkey) = split_key_path(key_path)?;
        if subkey.is_empty() {
            return Ok(());
        }
        let subkey = wide(subkey);
        let mut handle = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(subkey.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut handle,
                None,
            )
        };
        ensure_success(&format!("RegCreateKeyExW({key_path})"), status)?;
        drop(OwnedKey(handle));
        Ok(())
    }

    pub fn delete_key_verified(key_path: &str) -> Result<bool> {
        if !key_exists(key_path)? {
            return Ok(false);
        }
        let (root, subkey) = split_key_path(key_path)?;
        if subkey.is_empty() {
            bail!("refusing to delete a predefined registry root");
        }
        let subkey = wide(subkey);
        let status = unsafe { RegDeleteTreeW(root, PCWSTR(subkey.as_ptr())) };
        ensure_success(&format!("RegDeleteTreeW({key_path})"), status)?;
        if key_exists(key_path)? {
            bail!("registry key still exists after deletion: {key_path}");
        }
        Ok(true)
    }

    pub fn delete_value(key_path: &str, value_name: &str) -> Result<()> {
        if !key_exists(key_path)? {
            return Ok(());
        }
        if value_name.contains('\0') {
            bail!("registry value name contains NUL");
        }
        let key = open_key(key_path, KEY_SET_VALUE)?;
        let value_name = wide(value_name);
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(value_name.as_ptr())) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        ensure_success(&format!("RegDeleteValueW({key_path})"), status)
    }

    #[cfg(test)]
    mod tests {
        use super::split_key_path;

        #[test]
        fn registry_paths_accept_documented_root_aliases() {
            let (_, subkey) = split_key_path("HKLM\\Software\\LetRecovery").unwrap();
            assert_eq!(subkey, "Software\\LetRecovery");
            let (_, subkey) = split_key_path("HKEY_LOCAL_MACHINE\\Software\\LetRecovery").unwrap();
            assert_eq!(subkey, "Software\\LetRecovery");
        }

        #[test]
        fn registry_paths_reject_unknown_roots_and_nul() {
            assert!(split_key_path("UNKNOWN\\Software").is_err());
            assert!(split_key_path("HKLM\\Bad\0Key").is_err());
        }
    }
}

impl OfflineRegistry {
    /// Keep an offline Windows installation on its real bugcheck instead of turning an
    /// early boot failure into an opaque reboot loop. The loaded SYSTEM hive can contain
    /// more than one usable control set, so every existing CrashControl key is updated and
    /// read back through the Win32 registry boundary.
    pub fn disable_crash_auto_reboot_for_loaded_system(hive_name: &str) -> Result<Vec<u32>> {
        if hive_name.is_empty()
            || !hive_name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            anyhow::bail!("invalid loaded SYSTEM hive name")
        }

        let mut updated = Vec::new();
        for control_set in 1..=4 {
            let key = format!(
                "HKLM\\{}\\ControlSet{:03}\\Control\\CrashControl",
                hive_name, control_set
            );
            if !Self::key_exists(&key)? {
                continue;
            }
            Self::set_dword(&key, "AutoReboot", 0)?;
            if Self::query_dword(&key, "AutoReboot")? != 0 {
                anyhow::bail!("CrashControl AutoReboot readback mismatch for {key}")
            }
            updated.push(control_set);
        }
        if updated.is_empty() {
            anyhow::bail!("no existing CrashControl key was found in the loaded SYSTEM hive")
        }
        Ok(updated)
    }

    pub fn query_string(key_path: &str, value_name: &str) -> Result<String> {
        #[cfg(windows)]
        {
            native::query_string(key_path, value_name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn query_string_values_recursive(key_path: &str, value_name: &str) -> Result<Vec<String>> {
        #[cfg(windows)]
        {
            native::query_string_values_recursive(key_path, value_name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn subkey_names(key_path: &str) -> Result<Vec<String>> {
        #[cfg(windows)]
        {
            native::enumerate_subkeys(key_path)
        }
        #[cfg(not(windows))]
        {
            let _ = key_path;
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn query_dword(key_path: &str, value_name: &str) -> Result<u32> {
        #[cfg(windows)]
        {
            native::query_dword(key_path, value_name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn query_binary(key_path: &str, value_name: &str) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            native::query_binary(key_path, value_name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn key_exists(key_path: &str) -> Result<bool> {
        #[cfg(windows)]
        {
            native::key_exists(key_path)
        }
        #[cfg(not(windows))]
        {
            let _ = key_path;
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn load_hive(hive_name: &str, hive_file: &str) -> Result<()> {
        #[cfg(windows)]
        {
            native::load_hive(hive_name, hive_file)
        }
        #[cfg(not(windows))]
        {
            let _ = (hive_name, hive_file);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn unload_hive(hive_name: &str) -> Result<()> {
        #[cfg(windows)]
        {
            native::unload_hive(hive_name)
        }
        #[cfg(not(windows))]
        {
            let _ = hive_name;
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn set_dword(key_path: &str, value_name: &str, data: u32) -> Result<()> {
        #[cfg(windows)]
        {
            native::set_dword(key_path, value_name, data)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name, data);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn set_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        #[cfg(windows)]
        {
            native::set_string(key_path, value_name, data)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name, data);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn set_expand_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        #[cfg(windows)]
        {
            native::set_expand_string(key_path, value_name, data)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name, data);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn set_binary(key_path: &str, value_name: &str, data: &[u8]) -> Result<()> {
        #[cfg(windows)]
        {
            native::set_binary(key_path, value_name, data)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name, data);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    /// Historical compatibility entry point with fail-closed error propagation.
    pub fn delete_key(key_path: &str) -> Result<()> {
        Self::delete_key_verified(key_path)?;
        Ok(())
    }

    pub fn delete_key_verified(key_path: &str) -> Result<bool> {
        #[cfg(windows)]
        {
            native::delete_key_verified(key_path)
        }
        #[cfg(not(windows))]
        {
            let _ = key_path;
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn create_key(key_path: &str) -> Result<()> {
        #[cfg(windows)]
        {
            native::create_key(key_path)
        }
        #[cfg(not(windows))]
        {
            let _ = key_path;
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    pub fn delete_value(key_path: &str, value_name: &str) -> Result<()> {
        #[cfg(windows)]
        {
            native::delete_value(key_path, value_name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key_path, value_name);
            anyhow::bail!("Windows registry APIs are unavailable on this platform")
        }
    }

    /// Import a `.reg` file through Windows' own interchange-format parser.
    pub fn import_reg_file(reg_file: &str) -> Result<()> {
        let path = std::path::Path::new(reg_file);
        let metadata = path.symlink_metadata().map_err(|error| {
            anyhow::anyhow!(
                "failed to read registry import metadata for {}: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            anyhow::bail!("registry import file does not exist: {}", path.display());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0
                != 0
            {
                anyhow::bail!(
                    "registry import file must not be a reparse point: {}",
                    path.display()
                );
            }
        }
        let absolute = path.canonicalize().map_err(|error| {
            anyhow::anyhow!(
                "failed to resolve registry import path {}: {error}",
                path.display()
            )
        })?;
        let output = new_command("reg.exe")
            .arg("import")
            .arg(&absolute)
            .output()?;
        if !output.status.success() {
            let stdout = gbk_to_utf8(&output.stdout);
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("reg.exe import failed: {} {}", stdout.trim(), stderr.trim());
        }
        Ok(())
    }
}
