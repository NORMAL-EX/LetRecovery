//! Pure launch planning for the native system-backup UI.
//!
//! The planner mirrors the legacy routing rules but deliberately has no executor: it does not
//! inspect disks, download or install PE, write configuration, start a worker, or reboot.

use crate::core::install_config::BackupConfig;
use crate::download::config::OnlinePE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupLaunchMode {
    Direct,
    ViaPe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectBackupTaskKind {
    Wim { append_if_destination_exists: bool },
    Esd { append_if_destination_exists: bool },
    Swm { split_size_mb: u32 },
    Ghost,
}

#[derive(Debug, Clone)]
pub struct DirectBackupIntent {
    pub config: BackupConfig,
    pub capture_directory: String,
    pub task: DirectBackupTaskKind,
}

/// Request for the existing PE preparation/handoff pipeline.
///
/// Consumers must still perform cached-artifact verification immediately before use and write
/// the backup configuration through the existing safe handoff path.
#[derive(Debug, Clone)]
pub struct PeBackupPreparationIntent {
    pub config: BackupConfig,
    pub pe: OnlinePE,
}

#[derive(Debug, Clone)]
pub enum BackupLaunchIntent {
    Direct(DirectBackupIntent),
    ViaPe(PeBackupPreparationIntent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupLaunchPreview {
    pub mode: BackupLaunchMode,
    pub source_partition: String,
    pub destination: String,
    pub format: u8,
    pub incremental_requested: bool,
    pub requires_pe_preparation: bool,
    pub pe_display_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackupLaunchPlan {
    pub preview: BackupLaunchPreview,
    pub intent: BackupLaunchIntent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupPlanningError {
    SourcePartitionMissing,
    SavePathMissing,
    NameMissing,
    UnsupportedFormat(u8),
    InvalidSavePath,
    ExtensionMismatch { expected: &'static str },
    PeSelectionRequired,
}

impl std::fmt::Display for BackupPlanningError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourcePartitionMissing => formatter.write_str("备份源分区不能为空"),
            Self::SavePathMissing => formatter.write_str("备份保存路径不能为空"),
            Self::NameMissing => formatter.write_str("备份名称不能为空"),
            Self::UnsupportedFormat(format) => {
                write!(formatter, "不支持的备份格式编号: {format}")
            }
            Self::InvalidSavePath => {
                formatter.write_str(&crate::tr!("备份保存路径必须是绝对文件路径"))
            }
            Self::ExtensionMismatch { expected } => {
                formatter.write_str(&crate::tr!("备份保存路径必须使用 .{} 扩展名", expected))
            }
            Self::PeSelectionRequired => formatter.write_str("备份当前系统分区前必须选择 PE 环境"),
        }
    }
}

impl std::error::Error for BackupPlanningError {}

/// Matches the legacy route exactly: an already-running PE environment or a non-system source
/// can back up directly; the live system partition must be handed off through PE.
pub const fn decide_launch_mode(
    is_pe_environment: bool,
    source_is_system_partition: bool,
) -> BackupLaunchMode {
    if is_pe_environment || !source_is_system_partition {
        BackupLaunchMode::Direct
    } else {
        BackupLaunchMode::ViaPe
    }
}

/// Builds a side-effect-free launch plan from an already validated backup configuration.
pub fn plan_backup_launch(
    config: &BackupConfig,
    is_pe_environment: bool,
    source_is_system_partition: bool,
    selected_pe: Option<&OnlinePE>,
) -> Result<BackupLaunchPlan, BackupPlanningError> {
    validate_minimum_config(config)?;
    let mode = decide_launch_mode(is_pe_environment, source_is_system_partition);
    let pe_display_name = selected_pe.map(|pe| pe.display_name.clone());
    let preview = BackupLaunchPreview {
        mode,
        source_partition: config.source_partition.clone(),
        destination: config.save_path.clone(),
        format: config.format,
        incremental_requested: config.incremental,
        requires_pe_preparation: mode == BackupLaunchMode::ViaPe,
        pe_display_name,
    };

    let intent = match mode {
        BackupLaunchMode::Direct => BackupLaunchIntent::Direct(DirectBackupIntent {
            config: config.clone(),
            capture_directory: format!("{}\\", config.source_partition),
            task: direct_task(config)?,
        }),
        BackupLaunchMode::ViaPe => {
            let pe = selected_pe.ok_or(BackupPlanningError::PeSelectionRequired)?;
            BackupLaunchIntent::ViaPe(PeBackupPreparationIntent {
                config: config.clone(),
                pe: pe.clone(),
            })
        }
    };
    Ok(BackupLaunchPlan { preview, intent })
}

fn validate_minimum_config(config: &BackupConfig) -> Result<(), BackupPlanningError> {
    if config.source_partition.trim().is_empty() {
        return Err(BackupPlanningError::SourcePartitionMissing);
    }
    if config.save_path.trim().is_empty() {
        return Err(BackupPlanningError::SavePathMissing);
    }
    if config.name.trim().is_empty() {
        return Err(BackupPlanningError::NameMissing);
    }
    if config.format > 3 {
        return Err(BackupPlanningError::UnsupportedFormat(config.format));
    }
    let path = std::path::Path::new(config.save_path.trim());
    if !is_absolute_windows_file_path(config.save_path.trim()) || path.file_name().is_none() {
        return Err(BackupPlanningError::InvalidSavePath);
    }
    let expected = match config.format {
        0 => "wim",
        1 => "esd",
        2 => "swm",
        3 => "gho",
        _ => unreachable!(),
    };
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
    {
        return Err(BackupPlanningError::ExtensionMismatch { expected });
    }
    Ok(())
}

fn is_absolute_windows_file_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive_absolute = bytes.len() >= 4
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if drive_absolute {
        return true;
    }

    let Some(rest) = value
        .strip_prefix("\\\\")
        .or_else(|| value.strip_prefix("//"))
    else {
        return false;
    };
    let mut components = rest
        .split(['\\', '/'])
        .filter(|component| !component.is_empty());
    let server = components.next();
    let share = components.next();
    let file_components = components.collect::<Vec<_>>();
    server.is_some_and(|value| !matches!(value, "." | ".."))
        && share.is_some_and(|value| !matches!(value, "." | ".."))
        && file_components
            .last()
            .is_some_and(|value| !matches!(*value, "." | ".."))
}

fn direct_task(config: &BackupConfig) -> Result<DirectBackupTaskKind, BackupPlanningError> {
    match config.format {
        0 => Ok(DirectBackupTaskKind::Wim {
            append_if_destination_exists: config.incremental,
        }),
        1 => Ok(DirectBackupTaskKind::Esd {
            append_if_destination_exists: config.incremental,
        }),
        2 => Ok(DirectBackupTaskKind::Swm {
            split_size_mb: config.swm_split_size,
        }),
        3 => Ok(DirectBackupTaskKind::Ghost),
        format => Err(BackupPlanningError::UnsupportedFormat(format)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(format: u8) -> BackupConfig {
        let extension = match format {
            1 => "esd",
            2 => "swm",
            3 => "gho",
            _ => "wim",
        };
        BackupConfig {
            save_path: format!("D:\\backup.{extension}"),
            name: "System Backup".to_owned(),
            description: "Created by LetRecovery".to_owned(),
            source_partition: "C:".to_owned(),
            incremental: true,
            format,
            swm_split_size: 4096,
            wim_engine: 1,
        }
    }

    fn pe() -> OnlinePE {
        OnlinePE {
            download_url: "https://example.invalid/pe.wim".to_owned(),
            display_name: "LetRecovery PE".to_owned(),
            filename: "LetRecovery_PE.wim".to_owned(),
            md5: None,
            sha256: Some("00".repeat(32)),
        }
    }

    #[test]
    fn routing_matches_the_legacy_conditions() {
        assert_eq!(decide_launch_mode(true, true), BackupLaunchMode::Direct);
        assert_eq!(decide_launch_mode(true, false), BackupLaunchMode::Direct);
        assert_eq!(decide_launch_mode(false, false), BackupLaunchMode::Direct);
        assert_eq!(decide_launch_mode(false, true), BackupLaunchMode::ViaPe);
    }

    #[test]
    fn live_system_partition_requires_a_selected_pe() {
        assert!(matches!(
            plan_backup_launch(&config(0), false, true, None),
            Err(BackupPlanningError::PeSelectionRequired)
        ));
        let plan = plan_backup_launch(&config(0), false, true, Some(&pe())).unwrap();
        assert!(plan.preview.requires_pe_preparation);
        assert_eq!(
            plan.preview.pe_display_name.as_deref(),
            Some("LetRecovery PE")
        );
        let BackupLaunchIntent::ViaPe(intent) = plan.intent else {
            panic!("expected PE handoff intent");
        };
        assert_eq!(intent.config.wim_engine, 1);
        assert_eq!(intent.pe.filename, "LetRecovery_PE.wim");
    }

    #[test]
    fn destination_must_be_absolute_and_match_the_selected_format() {
        let mut value = config(0);
        value.save_path = "backup.wim".to_owned();
        assert!(matches!(
            plan_backup_launch(&value, false, false, None),
            Err(BackupPlanningError::InvalidSavePath)
        ));

        value.save_path = "D:\\backup.esd".to_owned();
        assert!(matches!(
            plan_backup_launch(&value, false, false, None),
            Err(BackupPlanningError::ExtensionMismatch { expected: "wim" })
        ));
    }

    #[test]
    fn unc_destination_remains_compatible_with_the_legacy_file_picker() {
        let mut value = config(0);
        value.save_path = "\\\\backup-server\\images\\system.wim".to_owned();
        let plan = plan_backup_launch(&value, false, false, None).unwrap();
        assert_eq!(
            plan.preview.destination,
            "\\\\backup-server\\images\\system.wim"
        );

        value.save_path = "\\\\backup-server\\images\\system.esd".to_owned();
        assert!(matches!(
            plan_backup_launch(&value, false, false, None),
            Err(BackupPlanningError::ExtensionMismatch { expected: "wim" })
        ));
    }

    #[test]
    fn direct_wim_and_esd_preserve_incremental_append_intent() {
        for format in [0, 1] {
            let plan = plan_backup_launch(&config(format), false, false, None).unwrap();
            let BackupLaunchIntent::Direct(intent) = plan.intent else {
                panic!("expected direct intent");
            };
            assert_eq!(intent.capture_directory, "C:\\");
            assert!(matches!(
                intent.task,
                DirectBackupTaskKind::Wim {
                    append_if_destination_exists: true
                } | DirectBackupTaskKind::Esd {
                    append_if_destination_exists: true
                }
            ));
        }
    }

    #[test]
    fn direct_swm_and_ghost_match_the_old_non_append_branches() {
        let swm = plan_backup_launch(&config(2), false, false, None).unwrap();
        let BackupLaunchIntent::Direct(swm) = swm.intent else {
            panic!("expected direct SWM intent");
        };
        assert_eq!(
            swm.task,
            DirectBackupTaskKind::Swm {
                split_size_mb: 4096
            }
        );

        let ghost = plan_backup_launch(&config(3), false, false, None).unwrap();
        let BackupLaunchIntent::Direct(ghost) = ghost.intent else {
            panic!("expected direct Ghost intent");
        };
        assert_eq!(ghost.task, DirectBackupTaskKind::Ghost);
    }

    #[test]
    fn invalid_config_is_rejected_without_creating_an_intent() {
        let mut invalid = config(4);
        assert!(matches!(
            plan_backup_launch(&invalid, false, false, None),
            Err(BackupPlanningError::UnsupportedFormat(4))
        ));
        invalid = config(0);
        invalid.save_path = "  ".to_owned();
        assert!(matches!(
            plan_backup_launch(&invalid, false, false, None),
            Err(BackupPlanningError::SavePathMissing)
        ));
    }
}
