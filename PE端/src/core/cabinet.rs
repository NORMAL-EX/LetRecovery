//! Windows Cabinet (`.cab`) extraction through SetupAPI.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use libloading::Library;

use crate::tr;

#[repr(transparent)]
#[derive(Clone, Copy, Default)]
struct BOOL(pub i32);

type UINT = u32;

type SpFileCallbackW = unsafe extern "system" fn(
    Context: *mut c_void,
    Notification: UINT,
    Param1: usize,
    Param2: usize,
) -> UINT;

type FnSetupIterateCabinetW = unsafe extern "system" fn(
    CabinetFile: *const u16,
    Reserved: u32,
    MsgHandler: SpFileCallbackW,
    Context: *mut c_void,
) -> BOOL;

const SPFILENOTIFY_FILEINCABINET: UINT = 0x0000_0011;
const SPFILENOTIFY_FILEEXTRACTED: UINT = 0x0000_0013;
const SPFILENOTIFY_NEEDNEWCABINET: UINT = 0x0000_0014;
const FILEOP_DOIT: UINT = 1;
const FILEOP_ABORT: UINT = 0;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

#[repr(C)]
struct FileInCabinetInfoW {
    NameInCabinet: *const u16,
    FileSize: u32,
    Win32Error: u32,
    DosDate: u16,
    DosTime: u16,
    DosAttribs: u16,
    FullTargetName: [u16; 260],
}

#[repr(C)]
struct FilePathsW {
    Target: *const u16,
    Source: *const u16,
    Win32Error: u32,
    Flags: u32,
}

unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }

    let mut len = 0;
    while *ptr.add(len) != 0 && len < 32_768 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

fn path_to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

struct ExtractContext {
    dest_dir: PathBuf,
    extracted_files: Vec<PathBuf>,
    requested_files: usize,
    error: Option<String>,
}

fn validated_target_path(dest_dir: &Path, name_in_cabinet: &str) -> Result<PathBuf> {
    let relative = Path::new(name_in_cabinet);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("cabinet entry uses an unsafe path: {name_in_cabinet}");
    }
    Ok(dest_dir.join(relative))
}

fn ensure_path_has_no_reparse_points(root: &Path, directory: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in directory
        .strip_prefix(root)
        .context("cabinet target escaped extraction directory")?
        .components()
    {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current)
            .with_context(|| format!("failed to inspect cabinet target directory {current:?}"))?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("cabinet target directory contains a reparse point: {current:?}");
        }
    }
    Ok(())
}

unsafe extern "system" fn cabinet_callback(
    context: *mut c_void,
    notification: UINT,
    param1: usize,
    _param2: usize,
) -> UINT {
    let Some(context) = (context as *mut ExtractContext).as_mut() else {
        return FILEOP_ABORT;
    };

    match notification {
        SPFILENOTIFY_FILEINCABINET => {
            let info = &mut *(param1 as *mut FileInCabinetInfoW);
            let name = wide_ptr_to_string(info.NameInCabinet);
            let target = match validated_target_path(&context.dest_dir, &name) {
                Ok(path) => path,
                Err(error) => {
                    context.error = Some(error.to_string());
                    return FILEOP_ABORT;
                }
            };
            let Some(parent) = target.parent() else {
                context.error = Some("cabinet target has no parent directory".to_string());
                return FILEOP_ABORT;
            };
            if let Err(error) = std::fs::create_dir_all(parent).and_then(|_| {
                ensure_path_has_no_reparse_points(&context.dest_dir, parent)
                    .map_err(std::io::Error::other)
            }) {
                context.error = Some(format!("failed to prepare cabinet target: {error}"));
                return FILEOP_ABORT;
            }

            let target_wide = path_to_wide(&target);
            if target_wide.len() > info.FullTargetName.len() {
                context.error = Some(format!("cabinet target path is too long: {target:?}"));
                return FILEOP_ABORT;
            }
            info.FullTargetName.fill(0);
            info.FullTargetName[..target_wide.len()].copy_from_slice(&target_wide);
            context.requested_files += 1;
            FILEOP_DOIT
        }
        SPFILENOTIFY_FILEEXTRACTED => {
            let paths = &*(param1 as *const FilePathsW);
            if paths.Win32Error != 0 {
                context.error = Some(format!(
                    "cabinet file extraction failed with Win32 error {}",
                    paths.Win32Error
                ));
                return FILEOP_ABORT;
            }
            context
                .extracted_files
                .push(PathBuf::from(wide_ptr_to_string(paths.Target)));
            FILEOP_DOIT
        }
        SPFILENOTIFY_NEEDNEWCABINET => {
            context.error = Some("multi-part cabinet archives are not supported".to_string());
            FILEOP_ABORT
        }
        _ => FILEOP_DOIT,
    }
}

pub struct CabinetExtractor {
    _lib: Library,
    iterate_cabinet: FnSetupIterateCabinetW,
}

impl CabinetExtractor {
    pub fn new() -> Result<Self> {
        let lib = unsafe { Library::new("setupapi.dll") }.context(tr!("无法加载 setupapi.dll"))?;
        unsafe {
            let iterate_cabinet: FnSetupIterateCabinetW = *lib.get(b"SetupIterateCabinetW")?;
            Ok(Self {
                _lib: lib,
                iterate_cabinet,
            })
        }
    }

    pub fn extract(&self, cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(dest_dir)?;
        let metadata = std::fs::symlink_metadata(dest_dir)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("cabinet extraction directory is a reparse point");
        }

        let mut context = ExtractContext {
            dest_dir: dest_dir.to_path_buf(),
            extracted_files: Vec::new(),
            requested_files: 0,
            error: None,
        };
        let cab_wide = path_to_wide(cab_path);
        let result = unsafe {
            (self.iterate_cabinet)(
                cab_wide.as_ptr(),
                0,
                cabinet_callback,
                (&mut context as *mut ExtractContext).cast(),
            )
        };

        if let Some(error) = context.error.take() {
            bail!("{error}");
        }
        if result.0 == 0 {
            bail!("{}", tr!("SetupIterateCabinetW 失败"));
        }
        if context.extracted_files.len() != context.requested_files {
            bail!(
                "cabinet extraction was incomplete: requested {}, extracted {}",
                context.requested_files,
                context.extracted_files.len()
            );
        }

        log::info!(
            "[CABINET] extracted {} files to {:?}",
            context.extracted_files.len(),
            dest_dir
        );
        Ok(context.extracted_files)
    }

    #[allow(
        dead_code,
        reason = "retained for PE extension and custom driver workflows"
    )]
    pub fn is_cab_file(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cab"))
    }
}

#[allow(
    dead_code,
    reason = "retained for PE extension and custom driver workflows"
)]
pub fn extract_cab(cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    CabinetExtractor::new()?.extract(cab_path, dest_dir)
}

#[allow(
    dead_code,
    reason = "retained for PE extension and custom driver workflows"
)]
pub fn extract_all_cabs(source_dir: &Path, dest_dir: &Path) -> Result<usize> {
    let extractor = CabinetExtractor::new()?;
    let mut count = 0;
    for entry in std::fs::read_dir(source_dir)? {
        let path = entry?.path();
        if CabinetExtractor::is_cab_file(&path) {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("cab");
            extractor.extract(&path, &dest_dir.join(name))?;
            count += 1;
        }
    }
    Ok(count)
}

#[allow(
    dead_code,
    reason = "retained for PE extension and custom driver workflows"
)]
pub fn find_cab_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| CabinetExtractor::is_cab_file(path))
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
        let root = Path::new(r"C:\extract");
        assert!(validated_target_path(root, r"drivers\disk.inf").is_ok());
        assert!(validated_target_path(root, r"..\outside.dll").is_err());
        assert!(validated_target_path(root, r"C:\outside.dll").is_err());
        assert!(validated_target_path(root, r"\\server\share\outside.dll").is_err());
    }
}
