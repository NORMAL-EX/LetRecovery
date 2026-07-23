//! Collision-resistant temporary regular files with best-effort cleanup.

use std::fs::{remove_dir_all, remove_file, File, OpenOptions};
use std::io::{self, Write};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);
const MAX_CREATE_ATTEMPTS: u64 = 128;

/// A temporary regular file that is removed when the guard is dropped.
///
/// `create_new` prevents one process instance from overwriting another one's
/// command script. Cleanup also runs when command startup or output handling
/// returns early with an error.
#[derive(Debug)]
pub struct ScopedTempFile {
    path: PathBuf,
}

/// A collision-resistant temporary directory removed recursively on drop.
#[derive(Debug)]
pub struct ScopedTempDir {
    path: PathBuf,
}

impl ScopedTempDir {
    pub fn create_in(parent: &Path, prefix: &str) -> io::Result<Self> {
        validate_name_component(prefix, "prefix")?;
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary directory",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the directory on disk and return its path.
    ///
    /// Use this only when ownership is intentionally transferred to another
    /// component that will clean the directory after consuming its contents.
    pub fn into_path(self) -> PathBuf {
        let this = ManuallyDrop::new(self);
        unsafe { std::ptr::read(&this.path) }
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = remove_dir_all(&self.path);
    }
}

impl ScopedTempFile {
    pub fn create_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
        contents: &[u8],
    ) -> io::Result<Self> {
        let (guard, mut file) = Self::create_writer_in(directory, prefix, extension)?;
        if let Err(error) = file.write_all(contents).and_then(|_| file.flush()) {
            drop(file);
            drop(guard);
            return Err(error);
        }
        drop(file);
        Ok(guard)
    }

    /// Allocate a unique temporary file and return both its cleanup guard and
    /// writable handle. Callers can stream large payloads without buffering
    /// them in memory; dropping the guard removes partial files on failure.
    pub fn create_writer_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
    ) -> io::Result<(Self, File)> {
        validate_name_component(prefix, "prefix")?;
        validate_name_component(extension, "extension")?;

        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let file_name = format!("{prefix}-{}-{id}.{extension}", std::process::id());
            let path = directory.join(file_name);
            let file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };

            return Ok((Self { path }, file));
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary file",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically publish this temporary file at `target`.
    ///
    /// The temporary file and target must be in the same directory so the
    /// replacement cannot degrade into a cross-volume copy. On Windows this
    /// uses `MoveFileExW` with replace and write-through semantics.
    pub fn persist_replace(self, target: &Path) -> io::Result<()> {
        if self.path.parent() != target.parent() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file and target must share a directory",
            ));
        }
        atomic_replace_path(&self.path, target)
    }
}

impl AsRef<Path> for ScopedTempFile {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl std::ops::Deref for ScopedTempFile {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

/// Atomically replace `target` with an already-complete file on the same
/// volume. Callers must keep `source` in a private staging directory and must
/// verify it before publishing.
#[cfg(windows)]
pub fn atomic_replace_path(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))
    }
}

/// Replace a file's inherited ACL with full-control entries for local SYSTEM
/// and BUILTIN\Administrators. This is intended for short-lived plaintext
/// secret files created by the elevated application.
#[cfg(windows)]
pub fn restrict_to_system_and_administrators(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
        SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
        SECURITY_MAX_SID_SIZE,
    };
    use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    struct AclGuard(*mut windows::Win32::Security::ACL);
    impl Drop for AclGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0.cast()));
                }
            }
        }
    }

    fn well_known_sid(kind: windows::Win32::Security::WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = buffer.len() as u32;
        unsafe {
            CreateWellKnownSid(
                kind,
                PSID::default(),
                PSID(buffer.as_mut_ptr().cast()),
                &mut size,
            )
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        }
        buffer.truncate(size as usize);
        Ok(buffer)
    }

    let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
    let system = well_known_sid(WinLocalSystemSid)?;
    let access = [administrators.as_ptr(), system.as_ptr()].map(|sid| EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS.0,
        grfAccessMode: SET_ACCESS,
        grfInheritance: Default::default(),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: PWSTR(sid.cast_mut().cast()),
        },
    });
    let mut acl = null_mut();
    let result = unsafe { SetEntriesInAclW(Some(&access), None, &mut acl) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    let _guard = AclGuard(acl);
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            PSID::default(),
            PSID::default(),
            Some(acl.cast_const()),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn restrict_to_system_and_administrators(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn atomic_replace_path(source: &Path, target: &Path) -> io::Result<()> {
    std::fs::rename(source, target)
}

impl Drop for ScopedTempFile {
    fn drop(&mut self) {
        let _ = remove_file(&self.path);
    }
}

fn validate_name_component(value: &str, field: &str) -> io::Result<()> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("temporary file {field} contains unsafe characters"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory() -> PathBuf {
        let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lr-core-temp-test-{}-{id}", std::process::id()))
    }

    #[test]
    fn creates_unique_files_and_cleans_them_on_drop() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();

        let first = ScopedTempFile::create_in(&directory, "diskpart", "txt", b"first").unwrap();
        let second = ScopedTempFile::create_in(&directory, "diskpart", "txt", b"second").unwrap();
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();

        assert_ne!(first_path, second_path);
        assert_eq!(std::fs::read(&first_path).unwrap(), b"first");
        assert_eq!(std::fs::read(&second_path).unwrap(), b"second");

        drop(first);
        drop(second);
        assert!(!first_path.exists());
        assert!(!second_path.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rejects_path_components_in_generated_names() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();

        let error =
            ScopedTempFile::create_in(&directory, "../script", "txt", b"unsafe").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn writer_api_cleans_partial_streams() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let (guard, mut writer) =
            ScopedTempFile::create_writer_in(&directory, "download", "wim").unwrap();
        writer.write_all(b"partial").unwrap();
        writer.flush().unwrap();
        let path = guard.path().to_path_buf();
        drop(writer);
        drop(guard);
        assert!(!path.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn persist_replace_atomically_replaces_an_existing_file() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("config.json");
        std::fs::write(&target, b"old").unwrap();
        let replacement = ScopedTempFile::create_in(&directory, "config", "json", b"new").unwrap();

        replacement.persist_replace(&target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn temp_dir_into_path_keeps_directory() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let temp = ScopedTempDir::create_in(&directory, "drivers").unwrap();
        let path = temp.into_path();

        assert!(path.is_dir());

        std::fs::remove_dir(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
