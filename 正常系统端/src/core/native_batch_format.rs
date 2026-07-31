//! Validated batch-format boundary for the native desktop UI.
//!
//! Validation always works from a fresh fixed-volume inventory. Execution
//! receives only a validated plan and formats each volume through the shared
//! parameterized VDS/WinAPI boundary.

use std::collections::HashSet;

use lr_core::format_command::FormatCommandSpec;

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

        let running_windows_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
        let partitions = DiskManager::get_partitions()
            .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
        let mut volumes = Vec::new();
        for partition in partitions.into_iter().filter(|partition| {
            !partition.is_system_partition
                && !partition
                    .letter
                    .chars()
                    .next()
                    .is_some_and(|letter| letter.eq_ignore_ascii_case(&running_windows_drive))
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
    let system_drive = format!(
        "{}:",
        lr_core::windows_storage::current_windows_drive_letter()
            .map_err(|error| BatchFormatError::Inventory(error.to_string()))?
    );
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

        if drive == system_drive {
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
        Ok(execute_with_formatter(plan, &WinApiVolumeFormatter))
    }
}

trait VolumeFormatter {
    fn format(&self, spec: &FormatCommandSpec) -> Result<(), String>;
}

#[cfg(not(feature = "non-elevated-tests"))]
struct WinApiVolumeFormatter;

#[cfg(not(feature = "non-elevated-tests"))]
impl VolumeFormatter for WinApiVolumeFormatter {
    fn format(&self, spec: &FormatCommandSpec) -> Result<(), String> {
        let drive_letter = spec
            .drive()
            .chars()
            .next()
            .ok_or_else(|| "validated volume is missing a drive letter".to_owned())?;
        let file_system = match spec.file_system() {
            lr_core::format_command::FileSystem::Ntfs => lr_core::windows_storage::FileSystem::Ntfs,
            lr_core::format_command::FileSystem::Fat => lr_core::windows_storage::FileSystem::Fat,
            lr_core::format_command::FileSystem::Fat32 => {
                lr_core::windows_storage::FileSystem::Fat32
            }
            lr_core::format_command::FileSystem::ExFat => {
                lr_core::windows_storage::FileSystem::ExFat
            }
        };
        lr_core::windows_storage::format_drive(
            drive_letter,
            file_system,
            spec.volume_label().unwrap_or_default(),
        )
        .map_err(|error| error.to_string())
    }
}

#[cfg(any(test, not(feature = "non-elevated-tests")))]
fn execute_with_formatter<F: VolumeFormatter + ?Sized>(
    plan: &ValidatedBatchFormatPlan,
    formatter: &F,
) -> BatchFormatExecutionResult {
    let mut volumes = Vec::with_capacity(plan.specs.len());
    for spec in &plan.specs {
        let result = match formatter.format(spec) {
            Ok(()) => BatchFormatVolumeResult {
                drive: spec.drive().to_string(),
                success: true,
                message: "format completed".to_owned(),
                exit_code: None,
            },
            Err(error) => BatchFormatVolumeResult {
                drive: spec.drive().to_string(),
                success: false,
                message: error,
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
    use std::sync::Mutex;

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
            validate_against_inventory(&request(&["D:"]), ["C:", "D:"], "D:"),
            Err(BatchFormatError::ProtectedDrive(drive)) if drive == "D:"
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

    struct SequencedFormatter {
        outcomes: Mutex<Vec<Result<(), String>>>,
        drives: Mutex<Vec<String>>,
    }

    impl VolumeFormatter for SequencedFormatter {
        fn format(&self, spec: &FormatCommandSpec) -> Result<(), String> {
            self.drives.lock().unwrap().push(spec.drive().to_owned());
            self.outcomes.lock().unwrap().remove(0)
        }
    }

    #[test]
    fn injected_formatter_preserves_per_volume_success_and_failure() {
        let plan = validate_against_inventory(&request(&["D:", "E:"]), ["D:", "E:"], "C:").unwrap();
        let formatter = SequencedFormatter {
            outcomes: Mutex::new(vec![Ok(()), Err("access denied".to_owned())]),
            drives: Mutex::new(Vec::new()),
        };

        let result = execute_with_formatter(&plan, &formatter);
        assert_eq!(result.success_count, 1);
        assert_eq!(result.fail_count, 1);
        assert_eq!(result.volumes[0].drive, "D:");
        assert!(result.volumes[0].success);
        assert_eq!(result.volumes[1].drive, "E:");
        assert!(!result.volumes[1].success);
        assert_eq!(
            *formatter.drives.lock().unwrap(),
            vec!["D:".to_owned(), "E:".to_owned()]
        );
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_feature_denies_before_any_format_api_call() {
        let plan = validate_against_inventory(&request(&["D:"]), ["D:"], "C:").unwrap();
        assert_eq!(
            execute(&plan).unwrap_err(),
            BatchFormatError::DevelopmentBuildDenied
        );
    }
}
