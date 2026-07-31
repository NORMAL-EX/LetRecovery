//! Safe wrapper around the documented Windows file-version APIs.

use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

impl fmt::Display for FileVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.revision
        )
    }
}

#[cfg(windows)]
pub fn query_file_version(path: &Path) -> anyhow::Result<FileVersion> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use anyhow::{bail, Context};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };

    if !path.is_file() {
        bail!("file does not exist: {}", path.display());
    }
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut ignored_handle = 0_u32;
    let size =
        unsafe { GetFileVersionInfoSizeW(PCWSTR(path_wide.as_ptr()), Some(&mut ignored_handle)) };
    if size == 0 {
        let status = unsafe { GetLastError() };
        bail!(
            "GetFileVersionInfoSizeW failed for {} with Win32 error {}",
            path.display(),
            status.0
        );
    }

    let mut data = vec![0_u8; size as usize];
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(path_wide.as_ptr()),
            0,
            size,
            data.as_mut_ptr().cast::<c_void>(),
        )
        .with_context(|| format!("GetFileVersionInfoW({})", path.display()))?;
    }

    let root = [b'\\' as u16, 0];
    let mut fixed_pointer = std::ptr::null_mut::<c_void>();
    let mut fixed_size = 0_u32;
    let found = unsafe {
        VerQueryValueW(
            data.as_ptr().cast::<c_void>(),
            PCWSTR(root.as_ptr()),
            &mut fixed_pointer,
            &mut fixed_size,
        )
    };
    if !found.as_bool()
        || fixed_pointer.is_null()
        || fixed_size < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
    {
        bail!("file has no valid VS_FIXEDFILEINFO: {}", path.display());
    }
    let fixed = unsafe { &*fixed_pointer.cast::<VS_FIXEDFILEINFO>() };
    if fixed.dwSignature != 0xFEEF_04BD {
        bail!(
            "file version signature is invalid for {}: 0x{:08X}",
            path.display(),
            fixed.dwSignature
        );
    }

    Ok(FileVersion {
        major: (fixed.dwFileVersionMS >> 16) as u16,
        minor: fixed.dwFileVersionMS as u16,
        build: (fixed.dwFileVersionLS >> 16) as u16,
        revision: fixed.dwFileVersionLS as u16,
    })
}

#[cfg(not(windows))]
pub fn query_file_version(_path: &Path) -> anyhow::Result<FileVersion> {
    anyhow::bail!("Windows file-version APIs are unavailable on this platform")
}

#[cfg(test)]
mod tests {
    use super::FileVersion;

    #[test]
    fn version_display_preserves_all_four_components() {
        assert_eq!(
            FileVersion {
                major: 10,
                minor: 0,
                build: 26100,
                revision: 1,
            }
            .to_string(),
            "10.0.26100.1"
        );
    }
}
