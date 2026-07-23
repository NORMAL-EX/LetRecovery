//! Validated native partition-copy boundary.

use super::partition_copy_impl as legacy_partition_copy;

use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionCopyRequest {
    pub source: String,
    pub target: String,
}

/// Read-only display inventory for the native partition-copy dialog.
///
/// This is deliberately separate from [`ValidatedPartitionCopyPlan`]: displaying a volume never
/// authorizes copying it. [`validate_current`] must still perform its fresh inventory and resume
/// marker checks after confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionCopyInventoryItem {
    pub drive: String,
    pub label: String,
    pub total_size_mb: u64,
    pub used_size_mb: u64,
    pub free_size_mb: u64,
    pub has_system: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedPartitionCopyPlan {
    source: String,
    target: String,
    resume: bool,
}

impl ValidatedPartitionCopyPlan {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub const fn resume(&self) -> bool {
        self.resume
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartitionCopyError {
    DevelopmentBuildDenied,
    InvalidDrive(String),
    SameVolume,
    VolumeNotAvailable(String),
    InsufficientSpace {
        required_mb: u64,
        available_mb: u64,
    },
    ConflictingResumeMarker {
        expected_source: String,
        marker_source: String,
    },
    Execution(String),
}

impl std::fmt::Display for PartitionCopyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => {
                formatter.write_str("partition copy is disabled in development-test builds")
            }
            Self::InvalidDrive(value) => write!(formatter, "invalid drive letter: {value:?}"),
            Self::SameVolume => formatter.write_str("source and target volumes must differ"),
            Self::VolumeNotAvailable(volume) => {
                write!(formatter, "volume {volume} is not in the fresh copy inventory")
            }
            Self::InsufficientSpace {
                required_mb,
                available_mb,
            } => write!(
                formatter,
                "target free space is insufficient: required {required_mb} MB, available {available_mb} MB"
            ),
            Self::ConflictingResumeMarker {
                expected_source,
                marker_source,
            } => write!(
                formatter,
                "target resume marker belongs to {marker_source}, not {expected_source}"
            ),
            Self::Execution(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for PartitionCopyError {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartitionCopyProgress {
    pub current_file: String,
    pub copied_count: usize,
    pub total_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub failed_files: Vec<String>,
    pub completed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionCopyExecutionResult {
    pub success: bool,
    pub partial_success: bool,
    pub resumed: bool,
    pub copied_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub total_count: usize,
    pub failed_files: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CopyVolume {
    drive: String,
    used_mb: u64,
    free_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ResumeMarker {
    Absent,
    Matching,
    Conflicting(String),
}

pub fn validate_current(
    request: &PartitionCopyRequest,
) -> Result<ValidatedPartitionCopyPlan, PartitionCopyError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = request;
        Err(PartitionCopyError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let normalized = PartitionCopyRequest {
            source: normalize_drive(&request.source)?,
            target: normalize_drive(&request.target)?,
        };
        if normalized.source == normalized.target {
            return Err(PartitionCopyError::SameVolume);
        }
        let inventory: Vec<_> = legacy_partition_copy::get_copyable_partitions()
            .into_iter()
            .map(|partition| CopyVolume {
                drive: partition.letter,
                used_mb: partition.used_size_mb,
                free_mb: partition.free_size_mb,
            })
            .collect();
        let marker = legacy_partition_copy::read_copy_marker(&normalized.target)
            .map(|marker| {
                if marker
                    .source_partition
                    .eq_ignore_ascii_case(&normalized.source)
                {
                    ResumeMarker::Matching
                } else {
                    ResumeMarker::Conflicting(marker.source_partition)
                }
            })
            .unwrap_or(ResumeMarker::Absent);
        validate_against_inventory(&normalized, &inventory, marker)
    }
}

/// Loads the same legacy copyable-volume inventory used by execution validation for display only.
/// Development-test builds fail before reading any host volume.
pub fn read_inventory() -> Result<Vec<PartitionCopyInventoryItem>, PartitionCopyError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        Err(PartitionCopyError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        Ok(inventory_from_legacy(
            legacy_partition_copy::get_copyable_partitions(),
        ))
    }
}

fn inventory_from_legacy(
    inventory: Vec<legacy_partition_copy::CopyablePartition>,
) -> Vec<PartitionCopyInventoryItem> {
    let mut seen = HashSet::new();
    inventory
        .into_iter()
        .filter_map(|partition| {
            let drive = normalize_drive(&partition.letter).ok()?;
            seen.insert(drive.clone())
                .then_some(PartitionCopyInventoryItem {
                    drive,
                    label: partition.label,
                    total_size_mb: partition.total_size_mb,
                    used_size_mb: partition.used_size_mb,
                    free_size_mb: partition.free_size_mb,
                    has_system: partition.has_system,
                })
        })
        .collect()
}

fn validate_against_inventory(
    request: &PartitionCopyRequest,
    inventory: &[CopyVolume],
    marker: ResumeMarker,
) -> Result<ValidatedPartitionCopyPlan, PartitionCopyError> {
    let source = normalize_drive(&request.source)?;
    let target = normalize_drive(&request.target)?;
    if source == target {
        return Err(PartitionCopyError::SameVolume);
    }
    let mut unique = HashSet::new();
    let inventory: Vec<_> = inventory
        .iter()
        .filter(|volume| unique.insert(volume.drive.to_ascii_uppercase()))
        .collect();
    let source_volume = inventory
        .iter()
        .find(|volume| volume.drive.eq_ignore_ascii_case(&source))
        .ok_or_else(|| PartitionCopyError::VolumeNotAvailable(source.clone()))?;
    let target_volume = inventory
        .iter()
        .find(|volume| volume.drive.eq_ignore_ascii_case(&target))
        .ok_or_else(|| PartitionCopyError::VolumeNotAvailable(target.clone()))?;
    if target_volume.free_mb < source_volume.used_mb {
        return Err(PartitionCopyError::InsufficientSpace {
            required_mb: source_volume.used_mb,
            available_mb: target_volume.free_mb,
        });
    }
    let resume = match marker {
        ResumeMarker::Absent => false,
        ResumeMarker::Matching => true,
        ResumeMarker::Conflicting(marker_source) => {
            return Err(PartitionCopyError::ConflictingResumeMarker {
                expected_source: source,
                marker_source,
            })
        }
    };
    Ok(ValidatedPartitionCopyPlan {
        source,
        target,
        resume,
    })
}

fn normalize_drive(value: &str) -> Result<String, PartitionCopyError> {
    let value = value.trim();
    if matches!(value.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) {
        Ok(value.to_ascii_uppercase())
    } else {
        Err(PartitionCopyError::InvalidDrive(value.to_string()))
    }
}

pub trait PartitionCopyRunner: Send + Sync {
    fn run(
        &self,
        plan: &ValidatedPartitionCopyPlan,
    ) -> Result<Vec<PartitionCopyProgress>, PartitionCopyError>;
}

pub fn execute(
    plan: &ValidatedPartitionCopyPlan,
) -> Result<PartitionCopyExecutionResult, PartitionCopyError> {
    execute_with(plan, &SystemPartitionCopyRunner)
}

pub fn execute_with_progress<F>(
    plan: &ValidatedPartitionCopyPlan,
    mut on_progress: F,
) -> Result<PartitionCopyExecutionResult, PartitionCopyError>
where
    F: FnMut(&PartitionCopyProgress),
{
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (plan, &mut on_progress);
        Err(PartitionCopyError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        legacy_partition_copy::execute_partition_copy(
            plan.source(),
            plan.target(),
            sender,
            plan.resume(),
        );
        let mut all = Vec::new();
        for progress in receiver {
            let progress = PartitionCopyProgress {
                current_file: progress.current_file,
                copied_count: progress.copied_count,
                total_count: progress.total_count,
                skipped_count: progress.skipped_count,
                failed_count: progress.failed_count,
                failed_files: progress.failed_files,
                completed: progress.completed,
                error: progress.error,
            };
            on_progress(&progress);
            all.push(progress);
        }
        summarize_progress(plan, &all)
    }
}

pub fn execute_with<R: PartitionCopyRunner + ?Sized>(
    plan: &ValidatedPartitionCopyPlan,
    runner: &R,
) -> Result<PartitionCopyExecutionResult, PartitionCopyError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (plan, runner);
        Err(PartitionCopyError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let progress = runner.run(plan)?;
        summarize_progress(plan, &progress)
    }
}

struct SystemPartitionCopyRunner;

impl PartitionCopyRunner for SystemPartitionCopyRunner {
    fn run(
        &self,
        plan: &ValidatedPartitionCopyPlan,
    ) -> Result<Vec<PartitionCopyProgress>, PartitionCopyError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = plan;
            Err(PartitionCopyError::DevelopmentBuildDenied)
        }
        #[cfg(not(feature = "non-elevated-tests"))]
        {
            let (sender, receiver) = std::sync::mpsc::channel();
            legacy_partition_copy::execute_partition_copy(
                plan.source(),
                plan.target(),
                sender,
                plan.resume(),
            );
            Ok(receiver
                .into_iter()
                .map(|progress| PartitionCopyProgress {
                    current_file: progress.current_file,
                    copied_count: progress.copied_count,
                    total_count: progress.total_count,
                    skipped_count: progress.skipped_count,
                    failed_count: progress.failed_count,
                    failed_files: progress.failed_files,
                    completed: progress.completed,
                    error: progress.error,
                })
                .collect())
        }
    }
}

fn summarize_progress(
    plan: &ValidatedPartitionCopyPlan,
    progress: &[PartitionCopyProgress],
) -> Result<PartitionCopyExecutionResult, PartitionCopyError> {
    let final_progress = progress
        .last()
        .ok_or_else(|| PartitionCopyError::Execution("copy produced no progress result".into()))?;
    if let Some(error) = &final_progress.error {
        return Err(PartitionCopyError::Execution(error.clone()));
    }
    if !final_progress.completed {
        return Err(PartitionCopyError::Execution(
            "copy ended without a completion state".into(),
        ));
    }
    let partial_success = final_progress.failed_count > 0;
    let success = !partial_success;
    Ok(PartitionCopyExecutionResult {
        success,
        partial_success,
        resumed: plan.resume,
        copied_count: final_progress.copied_count,
        skipped_count: final_progress.skipped_count,
        failed_count: final_progress.failed_count,
        total_count: final_progress.total_count,
        failed_files: final_progress.failed_files.clone(),
        message: if partial_success {
            format!(
                "partition copy completed with {} failed file(s)",
                final_progress.failed_count
            )
        } else if plan.resume {
            "partition copy resumed and completed".into()
        } else {
            "partition copy completed".into()
        },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn volumes() -> Vec<CopyVolume> {
        vec![
            CopyVolume {
                drive: "D:".into(),
                used_mb: 100,
                free_mb: 20,
            },
            CopyVolume {
                drive: "E:".into(),
                used_mb: 10,
                free_mb: 200,
            },
        ]
    }

    #[test]
    fn strict_plan_rejects_same_missing_space_and_conflicting_marker() {
        let inventory = volumes();
        let request = |source: &str, target: &str| PartitionCopyRequest {
            source: source.into(),
            target: target.into(),
        };
        assert_eq!(
            validate_against_inventory(&request("D:", "D:"), &inventory, ResumeMarker::Absent),
            Err(PartitionCopyError::SameVolume)
        );
        assert!(matches!(
            validate_against_inventory(&request("D:", "F:"), &inventory, ResumeMarker::Absent),
            Err(PartitionCopyError::VolumeNotAvailable(_))
        ));
        assert!(
            validate_against_inventory(&request("E:", "D:"), &inventory, ResumeMarker::Absent)
                .is_ok()
        );
        assert!(matches!(
            validate_against_inventory(
                &request("D:", "E:"),
                &inventory,
                ResumeMarker::Conflicting("F:".into())
            ),
            Err(PartitionCopyError::ConflictingResumeMarker { .. })
        ));
        let small_target = vec![
            inventory[0].clone(),
            CopyVolume {
                free_mb: 99,
                ..inventory[1].clone()
            },
        ];
        assert!(matches!(
            validate_against_inventory(&request("D:", "E:"), &small_target, ResumeMarker::Absent),
            Err(PartitionCopyError::InsufficientSpace { .. })
        ));
    }

    #[test]
    fn matching_marker_creates_resume_plan_and_partial_failure_is_not_success() {
        let plan = validate_against_inventory(
            &PartitionCopyRequest {
                source: "d:".into(),
                target: "e:".into(),
            },
            &volumes(),
            ResumeMarker::Matching,
        )
        .unwrap();
        assert!(plan.resume());
        let result = summarize_progress(
            &plan,
            &[PartitionCopyProgress {
                copied_count: 8,
                skipped_count: 2,
                failed_count: 1,
                total_count: 11,
                failed_files: vec!["Windows\\bad.sys: access denied".into()],
                completed: true,
                ..Default::default()
            }],
        )
        .unwrap();
        assert!(!result.success);
        assert!(result.partial_success);
        assert!(result.resumed);
        assert_eq!(result.failed_count, 1);
    }

    #[test]
    fn display_inventory_keeps_legacy_volume_details_and_deduplicates_drives() {
        let item = |letter: &str, label: &str, has_system: bool| {
            legacy_partition_copy::CopyablePartition {
                letter: letter.into(),
                label: label.into(),
                total_size_mb: 300,
                used_size_mb: 120,
                free_size_mb: 180,
                has_system,
                is_removable: false,
            }
        };
        let inventory = inventory_from_legacy(vec![
            item("d:", "Data", false),
            item("D:", "duplicate", true),
            item("E:", "Windows", true),
        ]);
        assert_eq!(inventory.len(), 2);
        assert_eq!(inventory[0].drive, "D:");
        assert_eq!(inventory[0].label, "Data");
        assert_eq!(inventory[0].total_size_mb, 300);
        assert_eq!(inventory[0].used_size_mb, 120);
        assert_eq!(inventory[0].free_size_mb, 180);
        assert!(!inventory[0].has_system);
        assert_eq!(inventory[1].drive, "E:");
        assert!(inventory[1].has_system);
    }

    struct CountingRunner(AtomicUsize);

    impl PartitionCopyRunner for CountingRunner {
        fn run(
            &self,
            _plan: &ValidatedPartitionCopyPlan,
        ) -> Result<Vec<PartitionCopyProgress>, PartitionCopyError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_denies_before_runner_or_inventory_io() {
        let plan = ValidatedPartitionCopyPlan {
            source: "D:".into(),
            target: "E:".into(),
            resume: false,
        };
        let runner = CountingRunner(AtomicUsize::new(0));
        assert_eq!(
            execute_with(&plan, &runner),
            Err(PartitionCopyError::DevelopmentBuildDenied)
        );
        assert_eq!(runner.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            read_inventory(),
            Err(PartitionCopyError::DevelopmentBuildDenied)
        );
        assert_eq!(
            validate_current(&PartitionCopyRequest {
                source: "D:".into(),
                target: "E:".into(),
            }),
            Err(PartitionCopyError::DevelopmentBuildDenied)
        );
    }
}
