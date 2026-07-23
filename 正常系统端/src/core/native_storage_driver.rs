//! Fixed-source safety boundary for the native storage-controller driver tool.
//!
//! The legacy tool accepts only an enumerated offline Windows volume and always imports from the
//! packaged `bin/drivers/storage_controller` directory. This module restores that contract: no
//! caller-provided directory or recursion option enters the plan. It performs no DISM call and no
//! driver write; a confirmed host may map a validated plan into the existing backend request.

use std::path::{Path, PathBuf};

pub const PACKAGED_STORAGE_DRIVER_RELATIVE_PATH: [&str; 3] =
    ["bin", "drivers", "storage_controller"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageDriverTarget {
    /// Canonical drive-root value such as `D:`. Display text must never replace this value.
    pub root: String,
    pub label: String,
}

impl StorageDriverTarget {
    pub fn new(root: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            label: label.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageDriverImportRequest {
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedStorageDriverImportPlan {
    target: String,
    driver_directory: PathBuf,
}

impl ValidatedStorageDriverImportPlan {
    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn driver_directory(&self) -> &Path {
        &self.driver_directory
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageDriverImportError {
    DevelopmentBuildDenied,
    InvalidTarget,
    ProtectedTarget(String),
    TargetNotInFreshInventory(String),
    SourceUnavailable(PathBuf),
    SourceInspection(String),
    ExecutableLocation(String),
}

impl std::fmt::Display for StorageDriverImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => formatter.write_str(
                "storage-driver import preparation is disabled in development-test builds",
            ),
            Self::InvalidTarget => formatter.write_str("offline Windows target is invalid"),
            Self::ProtectedTarget(target) => {
                write!(
                    formatter,
                    "the running Windows or PE volume cannot be targeted: {target}"
                )
            }
            Self::TargetNotInFreshInventory(target) => {
                write!(
                    formatter,
                    "target is not in the fresh offline Windows inventory: {target}"
                )
            }
            Self::SourceUnavailable(path) => write!(
                formatter,
                "packaged storage-controller driver directory is unavailable: {}",
                path.display()
            ),
            Self::SourceInspection(detail) | Self::ExecutableLocation(detail) => {
                formatter.write_str(detail)
            }
        }
    }
}

impl std::error::Error for StorageDriverImportError {}

/// Read-only source check injected so development builds can prove they reject before host I/O.
pub trait StorageDriverSourceProbe {
    fn is_directory(&self, path: &Path) -> Result<bool, String>;
}

pub struct FilesystemStorageDriverSourceProbe;

impl StorageDriverSourceProbe for FilesystemStorageDriverSourceProbe {
    fn is_directory(&self, path: &Path) -> Result<bool, String> {
        path.try_exists()
            .map(|exists| exists && path.is_dir())
            .map_err(|error| error.to_string())
    }
}

pub fn packaged_driver_directory(executable_directory: &Path) -> PathBuf {
    PACKAGED_STORAGE_DRIVER_RELATIVE_PATH
        .iter()
        .fold(executable_directory.to_path_buf(), |path, component| {
            path.join(component)
        })
}

/// Production preparation entry. It only checks the fixed packaged directory and fresh target
/// inventory; it never loads DISM or installs a driver.
pub fn prepare_current(
    request: &StorageDriverImportRequest,
    fresh_targets: &[StorageDriverTarget],
    current_system_drive: &str,
) -> Result<ValidatedStorageDriverImportPlan, StorageDriverImportError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (request, fresh_targets, current_system_drive);
        Err(StorageDriverImportError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let executable = std::env::current_exe()
            .map_err(|error| StorageDriverImportError::ExecutableLocation(error.to_string()))?;
        let executable_directory = executable.parent().ok_or_else(|| {
            StorageDriverImportError::ExecutableLocation(
                "the executable path has no parent directory".to_owned(),
            )
        })?;
        prepare_with_probe(
            request,
            fresh_targets,
            current_system_drive,
            executable_directory,
            &FilesystemStorageDriverSourceProbe,
        )
    }
}

/// Injectable preparation used by the host boundary and pure tests. Under the development
/// feature it rejects before consulting even the supplied probe.
pub fn prepare_with_probe<P: StorageDriverSourceProbe + ?Sized>(
    request: &StorageDriverImportRequest,
    fresh_targets: &[StorageDriverTarget],
    current_system_drive: &str,
    executable_directory: &Path,
    probe: &P,
) -> Result<ValidatedStorageDriverImportPlan, StorageDriverImportError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (
            request,
            fresh_targets,
            current_system_drive,
            executable_directory,
            probe,
        );
        Err(StorageDriverImportError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let target =
            normalize_drive(&request.target).ok_or(StorageDriverImportError::InvalidTarget)?;
        let system =
            normalize_drive(current_system_drive).ok_or(StorageDriverImportError::InvalidTarget)?;
        if target == "X:" || target == system {
            return Err(StorageDriverImportError::ProtectedTarget(target));
        }
        if !fresh_targets.iter().any(|candidate| {
            normalize_drive(&candidate.root).is_some_and(|candidate| candidate == target)
        }) {
            return Err(StorageDriverImportError::TargetNotInFreshInventory(target));
        }
        let driver_directory = packaged_driver_directory(executable_directory);
        match probe.is_directory(&driver_directory) {
            Ok(true) => Ok(ValidatedStorageDriverImportPlan {
                target,
                driver_directory,
            }),
            Ok(false) => Err(StorageDriverImportError::SourceUnavailable(
                driver_directory,
            )),
            Err(error) => Err(StorageDriverImportError::SourceInspection(error)),
        }
    }
}

fn normalize_drive(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches(['\\', '/']);
    match value.as_bytes() {
        [letter] if letter.is_ascii_alphabetic() => {
            Some(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            Some(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Probe {
        called: Cell<bool>,
        available: bool,
    }

    impl StorageDriverSourceProbe for Probe {
        fn is_directory(&self, _path: &Path) -> Result<bool, String> {
            self.called.set(true);
            Ok(self.available)
        }
    }

    fn request(target: &str) -> StorageDriverImportRequest {
        StorageDriverImportRequest {
            target: target.to_owned(),
        }
    }

    fn inventory() -> Vec<StorageDriverTarget> {
        vec![StorageDriverTarget::new("D:", "D: [Windows 11 24H2] [x64]")]
    }

    #[test]
    fn packaged_path_is_fixed_below_executable_directory() {
        assert_eq!(
            packaged_driver_directory(Path::new(r"E:\LetRecovery")),
            PathBuf::from(r"E:\LetRecovery\bin\drivers\storage_controller")
        );
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    #[test]
    fn plan_accepts_only_a_fresh_offline_target_and_fixed_source() {
        let probe = Probe {
            called: Cell::new(false),
            available: true,
        };
        let plan = prepare_with_probe(
            &request("d:\\"),
            &inventory(),
            "C:",
            Path::new(r"E:\LetRecovery"),
            &probe,
        )
        .unwrap();
        assert!(probe.called.get());
        assert_eq!(plan.target(), "D:");
        assert_eq!(
            plan.driver_directory(),
            Path::new(r"E:\LetRecovery\bin\drivers\storage_controller")
        );
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    #[test]
    fn target_and_source_fail_closed_before_any_driver_write_boundary() {
        for target in ["", "current", "C:", "X:", "E:"] {
            let probe = Probe {
                called: Cell::new(false),
                available: true,
            };
            assert!(prepare_with_probe(
                &request(target),
                &inventory(),
                "C:",
                Path::new(r"E:\LetRecovery"),
                &probe,
            )
            .is_err());
        }

        let missing = Probe {
            called: Cell::new(false),
            available: false,
        };
        assert!(matches!(
            prepare_with_probe(
                &request("D:"),
                &inventory(),
                "C:",
                Path::new(r"E:\LetRecovery"),
                &missing,
            ),
            Err(StorageDriverImportError::SourceUnavailable(_))
        ));

        let offline_c = [StorageDriverTarget::new("C:", "offline C")];
        let available = Probe {
            called: Cell::new(false),
            available: true,
        };
        assert!(prepare_with_probe(
            &request("C:"),
            &offline_c,
            "D:",
            Path::new(r"E:\LetRecovery"),
            &available,
        )
        .is_ok());
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_feature_rejects_before_source_probe() {
        let probe = Probe {
            called: Cell::new(false),
            available: true,
        };
        assert_eq!(
            prepare_with_probe(
                &request("D:"),
                &inventory(),
                "C:",
                Path::new(r"E:\LetRecovery"),
                &probe,
            ),
            Err(StorageDriverImportError::DevelopmentBuildDenied)
        );
        assert!(!probe.called.get());
    }
}
