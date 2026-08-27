//! Validated batch-format boundary for the native desktop UI.
//!
//! Validation always works from a fresh fixed-volume inventory. Execution
//! receives only a validated plan and formats each volume through the shared
//! parameterized VDS/WinAPI boundary.

use std::collections::{HashMap, HashSet};

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
    entries: Vec<ValidatedBatchFormatEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ValidatedBatchFormatEntry {
    spec: FormatCommandSpec,
    expected: lr_core::windows_storage::StableVolumeIdentity,
}

impl ValidatedBatchFormatPlan {
    pub fn drives(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(|entry| entry.spec.drive())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
    let system_letter = lr_core::windows_storage::current_windows_drive_letter()
        .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
    let system_drive = format!("{system_letter}:");
    let system_identity = lr_core::windows_storage::stable_volume_identity(system_letter)
        .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
    let allowed = partitions
        .iter()
        .filter(|partition| !partition.is_system_partition)
        .map(|partition| {
            let letter =
                partition.letter.chars().next().ok_or_else(|| {
                    BatchFormatError::Inventory("volume has no drive letter".into())
                })?;
            let identity = lr_core::windows_storage::stable_volume_identity(letter)
                .map_err(|error| BatchFormatError::Inventory(error.to_string()))?;
            Ok((partition.letter.as_str(), identity))
        })
        .collect::<Result<Vec<_>, BatchFormatError>>()?;
    validate_against_inventory(request, allowed, &system_drive, system_identity)
}

fn validate_against_inventory<'a>(
    request: &BatchFormatRequest,
    allowed_volumes: impl IntoIterator<Item = (&'a str, lr_core::windows_storage::StableVolumeIdentity)>,
    system_drive: &str,
    system_identity: lr_core::windows_storage::StableVolumeIdentity,
) -> Result<ValidatedBatchFormatPlan, BatchFormatError> {
    if request.drives.is_empty() {
        return Err(BatchFormatError::EmptySelection);
    }

    let system_drive = normalize_for_comparison(system_drive)?;
    let allowed = allowed_volumes
        .into_iter()
        .map(|(drive, identity)| normalize_for_comparison(drive).map(|drive| (drive, identity)))
        .collect::<Result<HashMap<_, _>, _>>()?;
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

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
        let Some(expected) = allowed.get(&drive).copied() else {
            return Err(BatchFormatError::DriveNotAllowed(drive));
        };
        if expected.extent.disk_number == system_identity.extent.disk_number
            && expected.extent.offset_bytes == system_identity.extent.offset_bytes
        {
            return Err(BatchFormatError::ProtectedDrive(drive));
        }
        if seen.insert(drive) {
            entries.push(ValidatedBatchFormatEntry { spec, expected });
        }
    }

    if entries.is_empty() {
        return Err(BatchFormatError::EmptySelection);
    }
    Ok(ValidatedBatchFormatPlan { entries })
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
    fn format(&self, entry: &ValidatedBatchFormatEntry) -> Result<(), String>;
}

#[cfg(not(feature = "non-elevated-tests"))]
struct WinApiVolumeFormatter;

#[cfg(not(feature = "non-elevated-tests"))]
impl VolumeFormatter for WinApiVolumeFormatter {
    fn format(&self, entry: &ValidatedBatchFormatEntry) -> Result<(), String> {
        let spec = &entry.spec;
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
        lr_core::windows_storage::format_drive_with_options_stable_checked(
            drive_letter,
            entry.expected,
            &lr_core::windows_storage::FormatOptions {
                file_system,
                label: spec.volume_label().unwrap_or_default().to_owned(),
                allocation_unit_size: 0,
                quick: true,
                force_dismount: false,
            },
        )
        .map_err(|error| error.to_string())
    }
}

#[cfg(any(test, not(feature = "non-elevated-tests")))]
fn execute_with_formatter<F: VolumeFormatter + ?Sized>(
    plan: &ValidatedBatchFormatPlan,
    formatter: &F,
) -> BatchFormatExecutionResult {
    let mut volumes = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        let spec = &entry.spec;
        let result = match formatter.format(entry) {
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

    fn identity(
        disk_number: u32,
        offset_bytes: u64,
    ) -> lr_core::windows_storage::StableVolumeIdentity {
        lr_core::windows_storage::StableVolumeIdentity {
            extent: lr_core::windows_storage::VolumeIdentity {
                disk_number,
                offset_bytes,
                extent_length_bytes: 64 * 1024 * 1024,
            },
            disk: lr_core::windows_storage::StableDiskIdentity::Gpt {
                disk_id: [disk_number as u8 + 1; 16],
            },
            partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                partition_id: [(offset_bytes / 4096) as u8 + 1; 16],
            },
            device_id_hash: Some([disk_number as u8 + 3; 32]),
        }
    }

    fn system_identity() -> lr_core::windows_storage::StableVolumeIdentity {
        identity(0, 1_048_576)
    }

    #[test]
    fn rejects_empty_protected_and_unlisted_volumes() {
        assert_eq!(
            validate_against_inventory(
                &request(&[]),
                [("D:", identity(1, 1_048_576))],
                "C:",
                system_identity(),
            )
            .unwrap_err(),
            BatchFormatError::EmptySelection
        );
        assert!(matches!(
            validate_against_inventory(
                &request(&["D:"]),
                [("C:", system_identity()), ("D:", identity(1, 1_048_576))],
                "D:",
                identity(1, 1_048_576),
            ),
            Err(BatchFormatError::ProtectedDrive(drive)) if drive == "D:"
        ));
        assert!(matches!(
            validate_against_inventory(
                &request(&["E:"]),
                [("D:", identity(1, 1_048_576))],
                "C:",
                system_identity(),
            ),
            Err(BatchFormatError::DriveNotAllowed(drive)) if drive == "E:"
        ));
    }

    #[test]
    fn normalizes_and_deduplicates_before_building_specs() {
        let plan = validate_against_inventory(
            &request(&["d", "D:\\", "e:"]),
            [
                ("D:", identity(1, 1_048_576)),
                ("E:", identity(2, 1_048_576)),
            ],
            "C:",
            system_identity(),
        )
        .unwrap();
        assert_eq!(plan.drives().collect::<Vec<_>>(), vec!["D:", "E:"]);
    }

    #[test]
    fn rejects_an_alias_of_the_running_windows_volume_by_physical_identity() {
        assert!(matches!(
            validate_against_inventory(
                &request(&["D:"]),
                [("D:", system_identity())],
                "C:",
                system_identity(),
            ),
            Err(BatchFormatError::ProtectedDrive(drive)) if drive == "D:"
        ));
    }

    #[test]
    fn format_spec_rejects_file_system_and_label_injection() {
        let mut invalid_fs = request(&["D:"]);
        invalid_fs.file_system = "NTFS /X".to_string();
        assert!(matches!(
            validate_against_inventory(
                &invalid_fs,
                [("D:", identity(1, 1_048_576))],
                "C:",
                system_identity(),
            ),
            Err(BatchFormatError::InvalidParameter(_))
        ));

        let mut invalid_label = request(&["D:"]);
        invalid_label.volume_label = "Data|whoami".to_string();
        assert!(matches!(
            validate_against_inventory(
                &invalid_label,
                [("D:", identity(1, 1_048_576))],
                "C:",
                system_identity(),
            ),
            Err(BatchFormatError::InvalidParameter(_))
        ));
    }

    struct SequencedFormatter {
        outcomes: Mutex<Vec<Result<(), String>>>,
        drives: Mutex<Vec<String>>,
        identities: Mutex<Vec<lr_core::windows_storage::StableVolumeIdentity>>,
    }

    impl VolumeFormatter for SequencedFormatter {
        fn format(&self, entry: &ValidatedBatchFormatEntry) -> Result<(), String> {
            self.drives
                .lock()
                .unwrap()
                .push(entry.spec.drive().to_owned());
            self.identities.lock().unwrap().push(entry.expected);
            self.outcomes.lock().unwrap().remove(0)
        }
    }

    #[test]
    fn injected_formatter_preserves_per_volume_success_and_failure() {
        let plan = validate_against_inventory(
            &request(&["D:", "E:"]),
            [
                ("D:", identity(1, 1_048_576)),
                ("E:", identity(2, 1_048_576)),
            ],
            "C:",
            system_identity(),
        )
        .unwrap();
        let formatter = SequencedFormatter {
            outcomes: Mutex::new(vec![Ok(()), Err("access denied".to_owned())]),
            drives: Mutex::new(Vec::new()),
            identities: Mutex::new(Vec::new()),
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
        assert_eq!(
            *formatter.identities.lock().unwrap(),
            vec![identity(1, 1_048_576), identity(2, 1_048_576)]
        );
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_feature_denies_before_any_format_api_call() {
        let plan = validate_against_inventory(
            &request(&["D:"]),
            [("D:", identity(1, 1_048_576))],
            "C:",
            system_identity(),
        )
        .unwrap();
        assert_eq!(
            execute(&plan).unwrap_err(),
            BatchFormatError::DevelopmentBuildDenied
        );
    }
}
