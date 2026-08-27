//! Shared native Windows diagnostics UI and Explorer file-selection boundary.
//!
//! `SHOpenFolderAndSelectItems` documents that a fully-qualified PIDL with an empty child array
//! opens the parent folder and selects that item. `SHParseDisplayName` is the documented preferred
//! string-to-PIDL conversion API. It can block, so parsing and Shell activation always happen on a
//! dedicated COM STA thread. The PIDL returned by the Shell is released with the COM task allocator
//! before that apartment is uninitialized.

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
};
#[cfg(windows)]
use windows::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_BUTTON,
    TDCBF_CLOSE_BUTTON, TDF_POSITION_RELATIVE_TO_WINDOW, TD_ERROR_ICON,
};
#[cfg(windows)]
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
#[cfg(windows)]
use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};

const OPEN_LOG_BUTTON_ID: i32 = 71_001;

/// Shows one modal error prompt with an application-defined “open file” button.
///
/// Returns `true` only when the custom button was pressed. The caller owns localization and decides
/// which already-published diagnostic file should be revealed.
#[cfg(windows)]
pub fn show_error_log_prompt(
    owner: HWND,
    window_title: &str,
    instruction: &str,
    content: &str,
    open_file_caption: &str,
) -> windows::core::Result<bool> {
    let window_title = wide(window_title);
    let instruction = wide(instruction);
    let content = wide(content);
    let open_file_caption = wide(open_file_caption);
    let button = TASKDIALOG_BUTTON {
        nButtonID: OPEN_LOG_BUTTON_ID,
        pszButtonText: PCWSTR(open_file_caption.as_ptr()),
    };
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_POSITION_RELATIVE_TO_WINDOW,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: PCWSTR(window_title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_ERROR_ICON,
        },
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: 1,
        pButtons: &button,
        nDefaultButton: OPEN_LOG_BUTTON_ID,
        ..Default::default()
    };
    let mut pressed = 0;
    // SAFETY: every string and the one-button array outlive the synchronous TaskDialog call; owner
    // is a top-level application window and all unused optional pointers remain null.
    unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None)? };
    Ok(pressed == OPEN_LOG_BUTTON_ID)
}

/// Opens Explorer with an existing ordinary file selected without constructing a command line.
///
/// Shell parsing is deliberately asynchronous because Microsoft documents that
/// `SHParseDisplayName` should run on a background thread. A fresh STA avoids inheriting an
/// incompatible apartment model from either UI implementation.
#[cfg(windows)]
pub fn reveal_file_in_explorer(path: PathBuf) -> std::io::Result<()> {
    validate_selectable_file(&path)?;
    std::thread::Builder::new()
        .name("lr-reveal-diagnostic".to_owned())
        .spawn(move || {
            if let Err(error) = reveal_file_worker(&path) {
                log::error!(
                    "[DIAGNOSTIC UI] failed to reveal diagnostic file {}: {error}",
                    path.display()
                );
            }
        })?;
    Ok(())
}

#[cfg(not(windows))]
pub fn reveal_file_in_explorer(_path: PathBuf) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Explorer file selection is only available on Windows",
    ))
}

fn validate_selectable_file(path: &Path) -> std::io::Result<()> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "diagnostic path is not absolute",
        ));
    }
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "diagnostic file does not exist",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn reveal_file_worker(path: &Path) -> windows::core::Result<()> {
    let com_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    com_result.ok()?;
    let _com = ComApartment;

    let display_name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut raw_pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: display_name is NUL terminated; the output starts null and is owned by Pidl after the
    // successful call. No attributes are requested, so both attribute arguments use zero/null.
    unsafe {
        SHParseDisplayName(PCWSTR(display_name.as_ptr()), None, &mut raw_pidl, 0, None)?;
    }
    let pidl = Pidl(raw_pidl);
    // Microsoft documents the `cidl == 0` form as opening the parent folder and selecting the
    // fully-qualified item represented by pidlFolder.
    unsafe { SHOpenFolderAndSelectItems(pidl.0, None, 0) }
}

#[cfg(windows)]
struct Pidl(*mut ITEMIDLIST);

#[cfg(windows)]
impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

#[cfg(windows)]
struct ComApartment;

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectable_file_validation_rejects_relative_and_missing_paths() {
        assert_eq!(
            validate_selectable_file(Path::new("relative.log"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        let missing = std::env::temp_dir().join(format!(
            "lr-missing-diagnostic-{}-{}.log",
            std::process::id(),
            u64::MAX
        ));
        assert_eq!(
            validate_selectable_file(&missing).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
