//! Validated batch-format boundary for the native desktop UI.
//!
//! Validation always works from a fresh fixed-volume inventory. Execution
//! receives only a validated plan and keeps every `format.com` argument
//! separate through `CommandRequest`.

use std::collections::HashSet;
use std::ffi::OsStr;

use lr_core::command::CommandExecutor;
#[cfg(not(feature = "non-elevated-tests"))]
use lr_core::command::SystemCommandExecutor;
#[cfg(not(feature = "non-elevated-tests"))]
use lr_core::format_command::system_format_executable;
use lr_core::format_command::FormatCommandSpec;
#[cfg(any(test, not(feature = "non-elevated-tests")))]
use lr_core::format_command::{output_indicates_error, output_indicates_success};

use super::disk::DiskManager;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchFormatRequest {
    pub drives: Vec<String>,
    pub file_system: String,
    pub volume_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchFormatInventoryVolume {
    pub drive: String,
    pub label: String,
    pub file_system: String,
    pub total_size_mb: u64,
    pub free_size_mb: u64,
}

pub fn inventory_current() -> Result<Vec<BatchFormatInventoryVolume>, BatchFormatError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        Err(BatchFormatError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetVolumeInformationW;

        let partitions = DiskManager::get_partitions()
            .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
        let mut volumes = Vec::new();
        for partition in partitions.into_iter().filter(|partition| {
            !partition.is_system_partition
                && !partition.letter.eq_ignore_ascii_case("C:")
                && !partition.letter.eq_ignore_ascii_case("X:")
        }) {
            let root = format!("{}\\", partition.letter);
            let root: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            let mut file_system = [0u16; 64];
            unsafe {
                GetVolumeInformationW(
                    PCWSTR(root.as_ptr()),
                    None,
                    None,
                    None,
                    None,
                    Some(&mut file_system),
                )
                .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
            }
            volumes.push(BatchFormatInventoryVolume {
                drive: partition.letter,
                label: partition.label,
                file_system: String::from_utf16_lossy(&file_system)
                    .trim_end_matches('\0')
                    .to_owned(),
                total_size_mb: partition.total_size_mb,
                free_size_mb: partition.free_size_mb,
            });
        }
        Ok(volumes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedBatchFormatPlan {
    specs: Vec<FormatCommandSpec>,
}

impl ValidatedBatchFormatPlan {
    pub fn drives(&self) -> impl ExactSizeIterator<Item = &str> {
        self.specs.iter().map(FormatCommandSpec::drive)
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchFormatError {
    EmptySelection,
    Inventory(String),
    InvalidParameter(String),
    ProtectedDrive(String),
    DriveNotAllowed(String),
    DevelopmentBuildDenied,
}

impl std::fmt::Display for BatchFormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySelection => formatter.write_str("no volumes were selected for formatting"),
            Self::Inventory(detail) | Self::InvalidParameter(detail) => formatter.write_str(detail),
            Self::ProtectedDrive(drive) => {
                write!(
                    formatter,
                    "formatting protected volume {drive} is forbidden"
                )
            }
            Self::DriveNotAllowed(drive) => {
                write!(
                    formatter,
                    "volume {drive} is not in the current format inventory"
                )
            }
            Self::DevelopmentBuildDenied => formatter
                .write_str("format execution is disabled in non-elevated development builds"),
        }
    }
}

impl std::error::Error for BatchFormatError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchFormatVolumeResult {
    pub drive: String,
    pub success: bool,
    pub message: String,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchFormatExecutionResult {
    pub success_count: usize,
    pub fail_count: usize,
    pub volumes: Vec<BatchFormatVolumeResult>,
}

/// Re-enumerate fixed volumes and produce the only plan accepted by the
/// production executor.
pub fn validate_current(
    request: &BatchFormatRequest,
) -> Result<ValidatedBatchFormatPlan, BatchFormatError> {
    let partitions = DiskManager::get_partitions()
        .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
    let allowed = partitions
        .iter()
        .filter(|partition| !partition.is_system_partition)
        .map(|partition| partition.letter.as_str());
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    validate_against_inventory(request, allowed, &system_drive)
}

fn validate_against_inventory<'a>(
    request: &BatchFormatRequest,
    allowed_drives: impl IntoIterator<Item = &'a str>,
    system_drive: &str,
) -> Result<ValidatedBatchFormatPlan, BatchFormatError> {
    if request.drives.is_empty() {
        return Err(BatchFormatError::EmptySelection);
    }

    let system_drive = normalize_for_comparison(system_drive)?;
    let allowed = allowed_drives
        .into_iter()
        .map(normalize_for_comparison)
        .collect::<Result<HashSet<_>, _>>()?;
    let mut seen = HashSet::new();
    let mut specs = Vec::new();

    for requested_drive in &request.drives {
        let spec = FormatCommandSpec::new(
            requested_drive,
            &request.file_system,
            Some(&request.volume_label),
        )
        .map_err(|error| BatchFormatError::InvalidParameter(error.to_string()))?;
        let drive = spec.drive().to_string();

        // C: and X: stay forbidden even when environment variables are stale
        // or the process is running from another Windows volume.
        if drive == "C:" || drive == "X:" || drive == system_drive {
            return Err(BatchFormatError::ProtectedDrive(drive));
        }
        if !allowed.contains(&drive) {
            return Err(BatchFormatError::DriveNotAllowed(drive));
        }
        if seen.insert(drive) {
            specs.push(spec);
        }
    }

    if specs.is_empty() {
        return Err(BatchFormatError::EmptySelection);
    }
    Ok(ValidatedBatchFormatPlan { specs })
}

fn normalize_for_comparison(drive: &str) -> Result<String, BatchFormatError> {
    FormatCommandSpec::new(drive, "NTFS", None)
        .map(|spec| spec.drive().to_string())
        .map_err(|error| BatchFormatError::InvalidParameter(error.to_string()))
}

pub fn execute(
    plan: &ValidatedBatchFormatPlan,
) -> Result<BatchFormatExecutionResult, BatchFormatError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = plan;
        Err(BatchFormatError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        execute_with(plan, &SystemCommandExecutor, system_format_executable())
    }
}

/// Injectable execution entry. In development-test builds it rejects before
/// invoking even the supplied executor, so an accidental system executor can
/// never start `format.com`.
pub fn execute_with<E, P>(
    plan: &ValidatedBatchFormatPlan,
    executor: &E,
    program: P,
) -> Result<BatchFormatExecutionResult, BatchFormatError>
where
    E: CommandExecutor + ?Sized,
    P: AsRef<OsStr>,
{
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (plan, executor, program.as_ref());
        Err(BatchFormatError::DevelopmentBuildDenied)
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    {
        Ok(execute_with_executor(plan, executor, program.as_ref()))
    }
}

#[cfg(any(test, not(feature = "non-elevated-tests")))]
fn execute_with_executor<E: CommandExecutor + ?Sized>(
    plan: &ValidatedBatchFormatPlan,
    executor: &E,
    program: &OsStr,
) -> BatchFormatExecutionResult {
    let mut volumes = Vec::with_capacity(plan.specs.len());
    for spec in &plan.specs {
        let request = spec.command_request(program);
        let result = match executor.execute(&request) {
            Ok(outcome) => {
                let stdout = lr_core::encoding::gbk_to_utf8(outcome.stdout());
                let stderr = lr_core::encoding::gbk_to_utf8(outcome.stderr());
                let has_error = output_indicates_error(outcome.succeeded(), &stdout, &stderr);
                let success =
                    (outcome.succeeded() || output_indicates_success(&stdout)) && !has_error;
                let message = if success {
                    "format completed".to_string()
                } else if !stderr.trim().is_empty() {
                    stderr.trim().to_string()
                } else if !stdout.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    "format failed without diagnostic output".to_string()
                };
                BatchFormatVolumeResult {
                    drive: spec.drive().to_string(),
                    success,
                    message,
                    exit_code: outcome.exit_code(),
                }
            }
            Err(error) => BatchFormatVolumeResult {
                drive: spec.drive().to_string(),
                success: false,
                message: format!("failed to start format.com: {error}"),
                exit_code: None,
            },
        };
        volumes.push(result);
    }

    let success_count = volumes.iter().filter(|result| result.success).count();
    BatchFormatExecutionResult {
        success_count,
        fail_count: volumes.len() - success_count,
        volumes,
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;

    use lr_core::command::{CommandOutcome, CommandRequest};

    use super::*;

    fn request(drives: &[&str]) -> BatchFormatRequest {
        BatchFormatRequest {
            drives: drives.iter().map(|drive| (*drive).to_string()).collect(),
            file_system: "NTFS".to_string(),
            volume_label: "Data".to_string(),
        }
    }

    #[test]
    fn rejects_empty_protected_and_unlisted_volumes() {
        assert_eq!(
            validate_against_inventory(&request(&[]), ["D:"], "C:").unwrap_err(),
            BatchFormatError::EmptySelection
        );
        assert!(matches!(
            validate_against_inventory(&request(&["C:"]), ["C:", "D:"], "C:"),
            Err(BatchFormatError::ProtectedDrive(drive)) if drive == "C:"
        ));
        assert!(matches!(
            validate_against_inventory(&request(&["X:"]), ["X:"], "D:"),
            Err(BatchFormatError::ProtectedDrive(drive)) if drive == "X:"
        ));
        assert!(matches!(
            validate_against_inventory(&request(&["E:"]), ["D:"], "C:"),
            Err(BatchFormatError::DriveNotAllowed(drive)) if drive == "E:"
        ));
    }

    #[test]
    fn normalizes_and_deduplicates_before_building_specs() {
        let plan =
            validate_against_inventory(&request(&["d", "D:\\", "e:"]), ["D:", "E:"], "C:").unwrap();
        assert_eq!(plan.drives().collect::<Vec<_>>(), vec!["D:", "E:"]);
    }

    #[test]
    fn format_spec_rejects_file_system_and_label_injection() {
        let mut invalid_fs = request(&["D:"]);
        invalid_fs.file_system = "NTFS /X".to_string();
        assert!(matches!(
            validate_against_inventory(&invalid_fs, ["D:"], "C:"),
            Err(BatchFormatError::InvalidParameter(_))
        ));

        let mut invalid_label = request(&["D:"]);
        invalid_label.volume_label = "Data|whoami".to_string();
        assert!(matches!(
            validate_against_inventory(&invalid_label, ["D:"], "C:"),
            Err(BatchFormatError::InvalidParameter(_))
        ));
    }

    struct SequencedExecutor {
        outcomes: Mutex<Vec<io::Result<CommandOutcome>>>,
        requests: Mutex<Vec<CommandRequest>>,
    }

    impl CommandExecutor for SequencedExecutor {
        fn execute(&self, request: &CommandRequest) -> io::Result<CommandOutcome> {
            self.requests.lock().unwrap().push(request.clone());
            self.outcomes.lock().unwrap().remove(0)
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    #[test]
    fn injected_executor_preserves_per_volume_success_and_failure() {
        let plan = validate_against_inventory(&request(&["D:", "E:"]), ["D:", "E:"], "C:").unwrap();
        let executor = SequencedExecutor {
            outcomes: Mutex::new(vec![
                Ok(CommandOutcome::success()),
                Ok(CommandOutcome::new(
                    Some(5),
                    Vec::new(),
                    b"access denied".to_vec(),
                )),
            ]),
            requests: Mutex::new(Vec::new()),
        };

        let result = execute_with(&plan, &executor, "format.com").unwrap();
        assert_eq!(result.success_count, 1);
        assert_eq!(result.fail_count, 1);
        assert_eq!(result.volumes[0].drive, "D:");
        assert!(result.volumes[0].success);
        assert_eq!(result.volumes[1].drive, "E:");
        assert!(!result.volumes[1].success);
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].arguments()[0], "D:");
        assert_eq!(requests[1].arguments()[0], "E:");
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_feature_denies_before_calling_injected_executor() {
        let plan = validate_against_inventory(&request(&["D:"]), ["D:"], "C:").unwrap();
        let executor = SequencedExecutor {
            outcomes: Mutex::new(vec![Ok(CommandOutcome::success())]),
            requests: Mutex::new(Vec::new()),
        };

        assert_eq!(
            execute_with(&plan, &executor, "format.com").unwrap_err(),
            BatchFormatError::DevelopmentBuildDenied
        );
        assert!(executor.requests.lock().unwrap().is_empty());
    }
}
