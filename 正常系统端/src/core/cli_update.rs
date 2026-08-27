//! Public normal-Windows CLI boundary for reversible Windows Update control.

use anyhow::{Context, Result};
use lr_core::offline_update_control::UpdateControlReport;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

fn validate_current_windows_root_candidate(root: &Path, expected_letter: char) -> Result<()> {
    let expected_letter = expected_letter.to_ascii_uppercase();
    if !('C'..='Z').contains(&expected_letter) {
        anyhow::bail!("running Windows drive letter is outside the supported C-Z range");
    }
    let expected = format!("{expected_letter}:\\");
    if !root
        .as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected)
    {
        anyhow::bail!(
            "update restore root is not the running Windows drive root: expected {expected}, got {}",
            root.display()
        );
    }
    Ok(())
}

fn current_windows_root() -> Result<PathBuf> {
    let letter = lr_core::windows_storage::current_windows_drive_letter()
        .map_err(|error| anyhow::anyhow!("locate the running Windows volume: {error}"))?;
    let root = PathBuf::from(format!("{letter}:\\"));
    validate_current_windows_root_candidate(&root, letter)?;
    let windows = root.join("Windows");
    let metadata = windows
        .symlink_metadata()
        .with_context(|| format!("inspect running Windows directory: {}", windows.display()))?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        anyhow::bail!("running Windows directory is missing or is a reparse point");
    }
    for hive in ["SYSTEM", "SOFTWARE"] {
        let path = windows.join("System32").join("config").join(hive);
        let metadata = path
            .symlink_metadata()
            .with_context(|| format!("inspect running registry hive: {}", path.display()))?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            anyhow::bail!(
                "running registry hive is missing, non-regular, or a reparse point: {}",
                path.display()
            );
        }
    }
    Ok(root)
}

/// Restores only values owned by LetRecovery's installation-bound manifest.
pub fn restore_current_windows_update() -> Result<UpdateControlReport> {
    log::info!("[UPDATE_RESTORE] status=started source=current_system_drive");
    let root = current_windows_root().map_err(|error| {
        log::error!(
            "[UPDATE_RESTORE] status=failed phase=validate_current_windows_root detail={error:#}"
        );
        error
    })?;
    let report =
        lr_core::offline_update_control::restore_online_update_control(&root).map_err(|error| {
            log::error!(
                "[UPDATE_RESTORE] status=failed phase=restore_online_registry detail={error:#}"
            );
            error
        })?;
    for warning in &report.warnings {
        log::warn!("[UPDATE_RESTORE] status=warning detail={warning}");
    }
    let status = if report.warnings.is_empty() && report.missing_services.is_empty() {
        "completed"
    } else {
        "completed_with_warnings"
    };
    log::info!(
        "[UPDATE_RESTORE] status={} restored={} already_restored={} missing_services={} warning_count={}",
        status,
        report.applied_values,
        report.already_applied_values,
        report.missing_services.len(),
        report.warnings.len()
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::validate_current_windows_root_candidate;
    use std::path::Path;

    #[test]
    fn current_root_validation_rejects_cross_system_paths() {
        validate_current_windows_root_candidate(Path::new("C:\\"), 'c')
            .expect("exact current root should pass");
        assert!(validate_current_windows_root_candidate(Path::new("D:\\"), 'C').is_err());
        assert!(validate_current_windows_root_candidate(Path::new("C:\\Windows"), 'C').is_err());
    }
}
