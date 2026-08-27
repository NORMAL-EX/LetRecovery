//! Recursive file-copy boundary using `CopyFileExW` for individual files.

use std::path::Path;

use sha2::{Digest, Sha256};

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

pub(crate) fn reject_existing_reparse_ancestors(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;

    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) if is_reparse_point(ancestor)? => {
                anyhow::bail!(
                    "copy path has a reparse-point/symbolic-link ancestor: {}",
                    ancestor.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect copy path ancestor: {}", ancestor.display())
                });
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use anyhow::{bail, Context};
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::CopyFileExW;
    use windows::Win32::System::WindowsProgramming::{
        COPY_FILE_FAIL_IF_EXISTS, COPY_FILE_RESTARTABLE,
    };

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
            COPY_FILE_RESTARTABLE | COPY_FILE_FAIL_IF_EXISTS,
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
    use anyhow::Context;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create copy destination: {}", parent.display()))?;
    }
    let mut input = std::fs::File::open(source)
        .with_context(|| format!("open copy source: {}", source.display()))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("create new copy destination: {}", destination.display()))?;
    std::io::copy(&mut input, &mut output)
        .with_context(|| format!("copy file into new destination: {}", destination.display()))?;
    output
        .sync_all()
        .with_context(|| format!("flush copied destination: {}", destination.display()))
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

    reject_existing_reparse_ancestors(source)?;
    reject_existing_reparse_ancestors(destination)?;
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
    reject_existing_reparse_ancestors(destination)?;
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
        let target_is_reparse_point = match std::fs::symlink_metadata(&target) {
            Ok(_) => match is_reparse_point(&target) {
                Ok(value) => value,
                Err(error) if continue_on_error => {
                    report.errors.push(error.to_string());
                    continue;
                }
                Err(error) => return Err(error),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) if continue_on_error => {
                report.errors.push(error.to_string());
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect copy destination: {}", target.display()));
            }
        };
        let result = if reparse_point {
            Err(anyhow::anyhow!(
                "refusing to follow reparse point/symbolic link: {}",
                entry.path().display()
            ))
        } else if target_is_reparse_point {
            Err(anyhow::anyhow!(
                "refusing to overwrite destination reparse point/symbolic link: {}",
                target.display()
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

fn sha256_file(path: &Path) -> anyhow::Result<[u8; 32]> {
    use anyhow::Context;
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open copied file for SHA-256: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read copied file for SHA-256: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Verify that every source entry has a same-type, non-reparse destination and
/// that every copied file has the same length and SHA-256.
pub fn verify_tree_copy(source: &Path, destination: &Path) -> anyhow::Result<usize> {
    use anyhow::{bail, Context};

    reject_existing_reparse_ancestors(source)?;
    reject_existing_reparse_ancestors(destination)?;
    if !source.is_dir() || is_reparse_point(source)? {
        bail!(
            "copy verification source must be a regular directory: {}",
            source.display()
        );
    }
    if !destination.is_dir() || is_reparse_point(destination)? {
        bail!(
            "copy verification destination must be a regular directory: {}",
            destination.display()
        );
    }

    let mut verified_files = 0_usize;
    let mut source_entries = 0_usize;
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry.context("enumerate copy verification source")?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .context("derive copy verification relative path")?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        source_entries += 1;
        if is_reparse_point(entry.path())? {
            bail!(
                "copy verification source contains a reparse point: {}",
                entry.path().display()
            );
        }
        let target = destination.join(relative);
        let target_metadata = std::fs::symlink_metadata(&target)
            .with_context(|| format!("inspect copied destination: {}", target.display()))?;
        if is_reparse_point(&target)? {
            bail!(
                "copied destination contains a reparse point: {}",
                target.display()
            );
        }
        if entry.file_type().is_dir() {
            if !target_metadata.is_dir() {
                bail!("copied directory has wrong type: {}", target.display());
            }
            continue;
        }
        if !entry.file_type().is_file() || !target_metadata.is_file() {
            bail!("copied file has wrong type: {}", target.display());
        }
        let source_metadata = entry
            .metadata()
            .with_context(|| format!("read copy source metadata: {}", entry.path().display()))?;
        if source_metadata.len() != target_metadata.len() {
            bail!(
                "copied file length mismatch: {} -> {}",
                entry.path().display(),
                target.display()
            );
        }
        if sha256_file(entry.path())? != sha256_file(&target)? {
            bail!(
                "copied file SHA-256 mismatch: {} -> {}",
                entry.path().display(),
                target.display()
            );
        }
        verified_files += 1;
    }
    let mut destination_entries = 0_usize;
    for entry in walkdir::WalkDir::new(destination).follow_links(false) {
        let entry = entry.context("enumerate copy verification destination")?;
        let relative = entry
            .path()
            .strip_prefix(destination)
            .context("derive destination verification relative path")?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        destination_entries += 1;
        if is_reparse_point(entry.path())? {
            bail!(
                "copy destination contains an unexpected reparse point: {}",
                entry.path().display()
            );
        }
        let source_entry = source.join(relative);
        let source_metadata = std::fs::symlink_metadata(&source_entry).with_context(|| {
            format!(
                "copy destination contains an entry absent from source: {}",
                entry.path().display()
            )
        })?;
        if source_metadata.is_dir() != entry.file_type().is_dir()
            || source_metadata.is_file() != entry.file_type().is_file()
        {
            bail!(
                "copy destination entry type differs from source: {}",
                entry.path().display()
            );
        }
    }
    if source_entries != destination_entries {
        bail!(
            "copy tree entry count mismatch: source {}, destination {}",
            source_entries,
            destination_entries
        );
    }
    Ok(verified_files)
}

/// Copy a tree and authoritatively re-read every source/destination file pair.
pub fn copy_tree_verified(source: &Path, destination: &Path) -> anyhow::Result<usize> {
    use anyhow::bail;

    let report = copy_tree(source, destination, false)?;
    if !report.errors.is_empty() {
        bail!(
            "copy unexpectedly completed with errors: {}",
            report.errors.join("; ")
        );
    }
    let verified_files = verify_tree_copy(source, destination)?;
    if verified_files != report.files_copied {
        bail!(
            "copied file count mismatch: copied {}, verified {}",
            report.files_copied,
            verified_files
        );
    }
    Ok(verified_files)
}

#[cfg(test)]
mod tests {
    use super::{copy_tree_verified, verify_tree_copy, CopyTreeReport};

    #[test]
    fn report_distinguishes_copied_files_from_errors() {
        let report = CopyTreeReport {
            files_copied: 2,
            errors: vec!["denied".to_owned()],
        };
        assert_eq!(report.files_copied, 2);
        assert_eq!(report.errors, ["denied"]);
    }

    #[test]
    fn verified_tree_copy_detects_changed_destination_bytes() {
        let root = std::env::temp_dir().join(format!(
            "lr-copy-verified-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("driver.inf"), b"driver").unwrap();
        std::fs::write(source.join("nested").join("driver.sys"), b"binary").unwrap();

        assert_eq!(copy_tree_verified(&source, &destination).unwrap(), 2);
        std::fs::write(destination.join("driver.inf"), b"tamper").unwrap();
        assert!(verify_tree_copy(&source, &destination).is_err());

        std::fs::write(destination.join("driver.inf"), b"driver").unwrap();
        std::fs::write(destination.join("unexpected.inf"), b"stale").unwrap();
        assert!(verify_tree_copy(&source, &destination).is_err());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_tree_copy_never_overwrites_an_existing_destination_file() {
        let root = std::env::temp_dir().join(format!(
            "lr-copy-exclusive-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("driver.inf"), b"new driver").unwrap();
        std::fs::write(destination.join("driver.inf"), b"existing data").unwrap();

        assert!(copy_tree_verified(&source, &destination).is_err());
        assert_eq!(
            std::fs::read(destination.join("driver.inf")).unwrap(),
            b"existing data"
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
