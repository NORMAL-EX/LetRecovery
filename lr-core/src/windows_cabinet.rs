//! Safe Windows Cabinet enumeration and extraction through `SetupIterateCabinetW`.
//!
//! The callback follows SetupAPI's notification-specific return contract:
//! `FILEOP_*` is returned only for `SPFILENOTIFY_FILEINCABINET`; later
//! notifications return `NO_ERROR` or a concrete Win32 error code.

use std::ffi::c_void;
use std::fs::File;
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupIterateCabinetW, FILEOP_ABORT, FILEOP_DOIT, FILEOP_SKIP, FILEPATHS_W,
    FILE_IN_CABINET_INFO_W, SPFILENOTIFY_FILEEXTRACTED, SPFILENOTIFY_FILEINCABINET,
    SPFILENOTIFY_NEEDNEWCABINET,
};
use windows::Win32::Foundation::{ERROR_INVALID_DATA, ERROR_NOT_SUPPORTED, NO_ERROR};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const MAX_CALLBACK_STRING_UNITS: usize = 32_768;

#[derive(Default)]
struct CabinetContext {
    destination: Option<PathBuf>,
    names: Vec<String>,
    extracted_files: Vec<PathBuf>,
    requested_files: usize,
    error: Option<String>,
}

fn wide_path(path: &Path) -> Result<Vec<u16>> {
    let encoded: Vec<u16> = path.as_os_str().encode_wide().collect();
    if encoded.contains(&0) {
        bail!("cabinet path contains an embedded NUL");
    }
    Ok(encoded.into_iter().chain(std::iter::once(0)).collect())
}

unsafe fn bounded_wide_string(pointer: *const u16) -> Result<String> {
    if pointer.is_null() {
        bail!("SetupAPI returned a null string");
    }
    let mut length = 0usize;
    while length < MAX_CALLBACK_STRING_UNITS && *pointer.add(length) != 0 {
        length += 1;
    }
    if length == MAX_CALLBACK_STRING_UNITS {
        bail!("SetupAPI returned an unterminated string");
    }
    String::from_utf16(std::slice::from_raw_parts(pointer, length))
        .context("SetupAPI returned invalid UTF-16")
}

fn regular_non_reparse_file(path: &Path, role: &str) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {role} {}", path.display()))?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!(
            "{role} must be a regular non-reparse file: {}",
            path.display()
        );
    }
    std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize {role} {}", path.display()))
}

fn canonical_destination(path: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create cabinet destination {}", path.display()))?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect cabinet destination {}", path.display()))?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!(
            "cabinet destination must be a non-reparse directory: {}",
            path.display()
        );
    }
    std::fs::canonicalize(path).with_context(|| {
        format!(
            "failed to canonicalize cabinet destination {}",
            path.display()
        )
    })
}

fn validated_relative_name(name: &str) -> Result<&Path> {
    let relative = Path::new(name);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("cabinet entry uses an unsafe path: {name}");
    }
    Ok(relative)
}

fn ensure_created_directories_are_not_reparse_points(root: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(root)
        .context("cabinet target escaped extraction directory")?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current).with_context(|| {
            format!("failed to inspect cabinet directory {}", current.display())
        })?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!(
                "cabinet target directory is not a regular directory: {}",
                current.display()
            );
        }
    }
    Ok(())
}

fn prepare_extraction_target(root: &Path, name: &str) -> Result<Vec<u16>> {
    let target = root.join(validated_relative_name(name)?);
    let parent = target
        .parent()
        .context("cabinet extraction target has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create cabinet directory {}", parent.display()))?;
    ensure_created_directories_are_not_reparse_points(root, parent)?;

    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!(
                "cabinet extraction target is not a regular file: {}",
                target.display()
            );
        }
    }
    wide_path(&target)
}

unsafe extern "system" fn cabinet_callback(
    context: *const c_void,
    notification: u32,
    param1: usize,
    _param2: usize,
) -> u32 {
    let Some(context) = (context as *mut CabinetContext).as_mut() else {
        return FILEOP_ABORT;
    };

    match notification {
        SPFILENOTIFY_FILEINCABINET => {
            if param1 == 0 {
                context.error = Some("SetupAPI returned a null cabinet entry".to_string());
                return FILEOP_ABORT;
            }
            let info = &mut *(param1 as *mut FILE_IN_CABINET_INFO_W);
            let name = match bounded_wide_string(info.NameInCabinet.as_ptr()) {
                Ok(name) => name,
                Err(error) => {
                    context.error = Some(error.to_string());
                    return FILEOP_ABORT;
                }
            };
            if let Err(error) = validated_relative_name(&name) {
                context.error = Some(error.to_string());
                return FILEOP_ABORT;
            }
            context.names.push(name.clone());

            let Some(destination) = context.destination.as_deref() else {
                return FILEOP_SKIP;
            };
            let target = match prepare_extraction_target(destination, &name) {
                Ok(target) => target,
                Err(error) => {
                    context.error = Some(error.to_string());
                    return FILEOP_ABORT;
                }
            };
            if target.len() > info.FullTargetName.len() {
                context.error = Some(format!(
                    "cabinet target path exceeds SetupAPI MAX_PATH: {}",
                    destination.join(name).display()
                ));
                return FILEOP_ABORT;
            }
            info.FullTargetName.fill(0);
            info.FullTargetName[..target.len()].copy_from_slice(&target);
            context.requested_files += 1;
            FILEOP_DOIT
        }
        SPFILENOTIFY_FILEEXTRACTED => {
            if param1 == 0 {
                context.error = Some("SetupAPI returned a null extraction result".to_string());
                return ERROR_INVALID_DATA.0;
            }
            let paths = &*(param1 as *const FILEPATHS_W);
            if paths.Win32Error != NO_ERROR.0 {
                context.error = Some(format!(
                    "cabinet file extraction failed with Win32 error {}",
                    paths.Win32Error
                ));
                return paths.Win32Error;
            }
            let target = match bounded_wide_string(paths.Target.as_ptr()) {
                Ok(target) => PathBuf::from(target),
                Err(error) => {
                    context.error = Some(error.to_string());
                    return ERROR_INVALID_DATA.0;
                }
            };
            if !context
                .destination
                .as_ref()
                .is_some_and(|destination| target.starts_with(destination))
            {
                context.error = Some(format!(
                    "SetupAPI reported a target outside the extraction directory: {}",
                    target.display()
                ));
                return ERROR_INVALID_DATA.0;
            }
            context.extracted_files.push(target);
            NO_ERROR.0
        }
        SPFILENOTIFY_NEEDNEWCABINET => {
            context.error = Some("multi-part cabinet archives are not supported".to_string());
            ERROR_NOT_SUPPORTED.0
        }
        _ => NO_ERROR.0,
    }
}

fn iterate_cabinet(cab_path: &Path, destination: Option<PathBuf>) -> Result<CabinetContext> {
    let cab_path = regular_non_reparse_file(cab_path, "cabinet source")?;
    let cabinet_wide = wide_path(&cab_path)?;
    let mut context = CabinetContext {
        destination,
        ..CabinetContext::default()
    };

    let result = unsafe {
        SetupIterateCabinetW(
            PCWSTR(cabinet_wide.as_ptr()),
            0,
            Some(cabinet_callback),
            (&mut context as *mut CabinetContext).cast(),
        )
    };
    if let Some(error) = context.error.take() {
        bail!("{error}");
    }
    result.context("SetupIterateCabinetW failed")?;
    Ok(context)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CabinetExtractor;

impl CabinetExtractor {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn extract(&self, cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
        let destination = canonical_destination(dest_dir)?;
        let context = iterate_cabinet(cab_path, Some(destination.clone()))?;
        if context.extracted_files.len() != context.requested_files {
            bail!(
                "cabinet extraction was incomplete: requested {}, extracted {}",
                context.requested_files,
                context.extracted_files.len()
            );
        }
        for path in &context.extracted_files {
            let metadata = std::fs::symlink_metadata(path)
                .with_context(|| format!("failed to verify extracted file {}", path.display()))?;
            if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                bail!("cabinet output is not a regular file: {}", path.display());
            }
        }
        log::info!(
            "[CABINET] extracted {} files to {} with SetupIterateCabinetW",
            context.extracted_files.len(),
            destination.display()
        );
        Ok(context.extracted_files)
    }

    pub fn list_contents(&self, cab_path: &Path) -> Result<Vec<String>> {
        Ok(iterate_cabinet(cab_path, None)?.names)
    }

    pub fn is_cab_file(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cab"))
    }

    pub fn is_valid_cab_file(path: &Path) -> bool {
        let Ok(path) = regular_non_reparse_file(path, "cabinet source") else {
            return false;
        };
        let Ok(mut file) = File::open(path) else {
            return false;
        };
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).is_ok() && &magic == b"MSCF"
    }
}

pub fn extract_cab(cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    CabinetExtractor::new()?.extract(cab_path, dest_dir)
}

pub fn extract_all_cabs(source_dir: &Path, dest_dir: &Path) -> Result<usize> {
    let extractor = CabinetExtractor::new()?;
    std::fs::create_dir_all(dest_dir)?;
    let mut count = 0usize;
    for cab_path in find_cab_files(source_dir) {
        let name = cab_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("cab");
        match extractor.extract(&cab_path, &dest_dir.join(name)) {
            Ok(files) => {
                log::info!(
                    "[CABINET] extracted {} files from {}",
                    files.len(),
                    cab_path.display()
                );
                count += 1;
            }
            Err(error) => {
                log::error!(
                    "[CABINET] failed to extract {}: {error:#}",
                    cab_path.display()
                );
            }
        }
    }
    Ok(count)
}

pub fn find_cab_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| CabinetExtractor::is_cab_file(path))
        .filter(|path| regular_non_reparse_file(path, "cabinet source").is_ok())
        .collect()
}

pub fn find_cab_files_recursive(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| CabinetExtractor::is_cab_file(path))
        .filter(|path| regular_non_reparse_file(path, "cabinet source").is_ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_cabinet_extension_case_insensitively() {
        assert!(CabinetExtractor::is_cab_file(Path::new("test.cab")));
        assert!(CabinetExtractor::is_cab_file(Path::new("test.CAB")));
        assert!(!CabinetExtractor::is_cab_file(Path::new("test.inf")));
    }

    #[test]
    fn cabinet_entry_path_rejects_traversal_and_absolute_names() {
        assert!(validated_relative_name(r"drivers\disk.inf").is_ok());
        assert!(validated_relative_name(r"..\outside.dll").is_err());
        assert!(validated_relative_name(r"C:\outside.dll").is_err());
        assert!(validated_relative_name(r"\\server\share\outside.dll").is_err());
    }

    #[test]
    fn callback_constants_match_the_documented_notification_contract() {
        assert_eq!(FILEOP_ABORT, 0);
        assert_eq!(FILEOP_DOIT, 1);
        assert_eq!(FILEOP_SKIP, 2);
        assert_eq!(NO_ERROR.0, 0);
    }
}
