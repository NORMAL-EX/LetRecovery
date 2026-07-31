//! Recursive file-copy boundary using `CopyFileExW` for individual files.

use std::path::Path;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CopyTreeReport {
    pub files_copied: usize,
    pub errors: Vec<String>,
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> anyhow::Result<bool> {
    use std::os::windows::fs::MetadataExt;

    use anyhow::Context;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("read copy-source metadata: {}", path.display()))?;
    Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0)
}

#[cfg(not(windows))]
fn is_reparse_point(path: &Path) -> anyhow::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_symlink())
}

#[cfg(windows)]
fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use anyhow::{bail, Context};
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::CopyFileExW;
    use windows::Win32::System::WindowsProgramming::COPY_FILE_RESTARTABLE;

    if !source.is_file() {
        bail!("copy source is not a regular file: {}", source.display());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create copy destination: {}", parent.display()))?;
    }
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        CopyFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            None,
            None,
            None,
            COPY_FILE_RESTARTABLE,
        )
        .with_context(|| {
            format!(
                "CopyFileExW({} -> {})",
                source.display(),
                destination.display()
            )
        })
    }
}

#[cfg(not(windows))]
fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::copy(source, destination)
        .map(|_| ())
        .map_err(Into::into)
}

/// Copy a directory tree without following reparse points/symbolic links.
///
/// When `continue_on_error` is true, per-entry failures are collected in the
/// report so callers can apply their own authoritative post-copy verification.
pub fn copy_tree(
    source: &Path,
    destination: &Path,
    continue_on_error: bool,
) -> anyhow::Result<CopyTreeReport> {
    use anyhow::{bail, Context};

    if !source.is_dir() {
        bail!("copy source is not a directory: {}", source.display());
    }
    if is_reparse_point(source)? {
        bail!(
            "copy source root must not be a reparse point: {}",
            source.display()
        );
    }
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create copy root: {}", destination.display()))?;
    if is_reparse_point(destination)? {
        bail!(
            "copy destination root must not be a reparse point: {}",
            destination.display()
        );
    }

    let mut report = CopyTreeReport::default();
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if continue_on_error => {
                report.errors.push(error.to_string());
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let relative = entry
            .path()
            .strip_prefix(source)
            .context("derive copy-tree relative path")?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        let reparse_point = match is_reparse_point(entry.path()) {
            Ok(value) => value,
            Err(error) if continue_on_error => {
                report.errors.push(error.to_string());
                continue;
            }
            Err(error) => return Err(error),
        };
        let result = if reparse_point {
            Err(anyhow::anyhow!(
                "refusing to follow reparse point/symbolic link: {}",
                entry.path().display()
            ))
        } else if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("create copied directory: {}", target.display()))
        } else if entry.file_type().is_file() {
            copy_file(entry.path(), &target).map(|()| {
                report.files_copied += 1;
            })
        } else {
            Err(anyhow::anyhow!(
                "unsupported filesystem entry: {}",
                entry.path().display()
            ))
        };
        if let Err(error) = result {
            if continue_on_error {
                report.errors.push(error.to_string());
            } else {
                return Err(error);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::CopyTreeReport;

    #[test]
    fn report_distinguishes_copied_files_from_errors() {
        let report = CopyTreeReport {
            files_copied: 2,
            errors: vec!["denied".to_owned()],
        };
        assert_eq!(report.files_copied, 2);
        assert_eq!(report.errors, ["denied"]);
    }
}
