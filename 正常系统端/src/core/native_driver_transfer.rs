//! Pure state and validation for the native driver export/import dialog.
//!
//! This module never calls DISM, reads a driver directory, or mutates Windows. It converts the
//! user's selected mode, Windows target and directory into a typed intent for the existing tool
//! execution boundary.

use super::native_tool_inventory::InventoryEntry;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DriverTransferMode {
    #[default]
    Export,
    Import,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverDirectoryRole {
    ExportDestination,
    ImportSource,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriverTransferState {
    pub mode: DriverTransferMode,
    pub windows_targets: Vec<InventoryEntry>,
    pub selected_windows: Option<String>,
    pub directory: String,
    pub inventory_loading: bool,
    pub status: String,
}

impl DriverTransferState {
    pub const fn directory_role(&self) -> DriverDirectoryRole {
        match self.mode {
            DriverTransferMode::Export => DriverDirectoryRole::ExportDestination,
            DriverTransferMode::Import => DriverDirectoryRole::ImportSource,
        }
    }

    pub fn selected_label(&self) -> Option<&str> {
        let selected = self.selected_windows.as_deref()?;
        self.windows_targets
            .iter()
            .find(|entry| entry.value.eq_ignore_ascii_case(selected))
            .map(|entry| entry.label.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverTransferRequest {
    pub mode: DriverTransferMode,
    pub windows_root: String,
    pub directory: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverTransferIntent {
    BrowseDirectory(DriverDirectoryRole),
    Execute(DriverTransferRequest),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverTransferValidationError {
    InventoryLoading,
    MissingWindowsTarget,
    StaleWindowsTarget,
    MissingDirectory,
}

impl std::fmt::Display for DriverTransferValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InventoryLoading => crate::tr!("正在检测 Windows 分区，请稍候"),
            Self::MissingWindowsTarget => crate::tr!("请选择系统分区"),
            Self::StaleWindowsTarget => crate::tr!("所选系统分区已不可用，请重新选择"),
            Self::MissingDirectory => crate::tr!("请指定目录路径"),
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for DriverTransferValidationError {}

pub fn build_execute_intent(
    state: &DriverTransferState,
) -> Result<DriverTransferIntent, DriverTransferValidationError> {
    if state.inventory_loading {
        return Err(DriverTransferValidationError::InventoryLoading);
    }
    let selected = state
        .selected_windows
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(DriverTransferValidationError::MissingWindowsTarget)?;
    let target = state
        .windows_targets
        .iter()
        .find(|entry| entry.value.eq_ignore_ascii_case(selected))
        .ok_or(DriverTransferValidationError::StaleWindowsTarget)?;
    let directory = state.directory.trim();
    if directory.is_empty() {
        return Err(DriverTransferValidationError::MissingDirectory);
    }
    Ok(DriverTransferIntent::Execute(DriverTransferRequest {
        mode: state.mode,
        windows_root: target.value.clone(),
        directory: directory.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> Vec<InventoryEntry> {
        vec![InventoryEntry {
            value: String::from("D:"),
            label: String::from("D: [Windows 11] [x64]"),
            disk_fingerprint: None,
        }]
    }

    fn valid(mode: DriverTransferMode) -> DriverTransferState {
        DriverTransferState {
            mode,
            windows_targets: targets(),
            selected_windows: Some(String::from("D:")),
            directory: String::from(" C:\\Drivers "),
            ..Default::default()
        }
    }

    #[test]
    fn export_and_import_preserve_the_same_required_windows_selection() {
        for mode in [DriverTransferMode::Export, DriverTransferMode::Import] {
            let intent = build_execute_intent(&valid(mode)).unwrap();
            assert_eq!(
                intent,
                DriverTransferIntent::Execute(DriverTransferRequest {
                    mode,
                    windows_root: String::from("D:"),
                    directory: String::from("C:\\Drivers"),
                })
            );
        }
    }

    #[test]
    fn mode_selects_the_original_conditional_directory_role() {
        assert_eq!(
            valid(DriverTransferMode::Export).directory_role(),
            DriverDirectoryRole::ExportDestination
        );
        assert_eq!(
            valid(DriverTransferMode::Import).directory_role(),
            DriverDirectoryRole::ImportSource
        );
    }

    #[test]
    fn loading_missing_stale_and_empty_inputs_are_rejected_without_io() {
        let mut state = valid(DriverTransferMode::Export);
        state.inventory_loading = true;
        assert_eq!(
            build_execute_intent(&state),
            Err(DriverTransferValidationError::InventoryLoading)
        );
        state.inventory_loading = false;
        state.selected_windows = None;
        assert_eq!(
            build_execute_intent(&state),
            Err(DriverTransferValidationError::MissingWindowsTarget)
        );
        state.selected_windows = Some(String::from("X:"));
        assert_eq!(
            build_execute_intent(&state),
            Err(DriverTransferValidationError::StaleWindowsTarget)
        );
        state.selected_windows = Some(String::from("D:"));
        state.directory.clear();
        assert_eq!(
            build_execute_intent(&state),
            Err(DriverTransferValidationError::MissingDirectory)
        );
    }
}
