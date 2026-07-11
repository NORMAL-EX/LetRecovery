//! Collision-resistant temporary regular files with best-effort cleanup.

use std::fs::{remove_file, File, OpenOptions};
use std::io::{self, Write};
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
}
