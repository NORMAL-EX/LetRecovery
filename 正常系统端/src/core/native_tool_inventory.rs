//! Read-only inventory loaders for native mutating-tool dialogs.

#[cfg(not(feature = "non-elevated-tests"))]
use super::windows_version_detect as version_detect;

#[cfg(not(feature = "non-elevated-tests"))]
use lr_core::command::{CommandExecutor, CommandRequest, SystemCommandExecutor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryEntry {
    pub value: String,
    pub label: String,
    pub disk_fingerprint: Option<super::native_quick_partition::DiskFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicInventoryKind {
    ResetPasswordAccounts,
    RemoveAppxPackages,
    NvidiaDevices,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeToolInventoryError {
    DevelopmentBuildDenied,
    InvalidTarget,
    Read(String),
}

impl std::fmt::Display for NativeToolInventoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => {
                formatter.write_str("tool inventory is disabled in development-test builds")
            }
            Self::InvalidTarget => formatter.write_str("invalid inventory target"),
            Self::Read(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for NativeToolInventoryError {}

pub fn load_dynamic(
    kind: DynamicInventoryKind,
    target: &str,
) -> Result<Vec<InventoryEntry>, NativeToolInventoryError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (kind, target);
        Err(NativeToolInventoryError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        match kind {
            DynamicInventoryKind::ResetPasswordAccounts => load_accounts(target),
            DynamicInventoryKind::RemoveAppxPackages => load_appx(target),
            DynamicInventoryKind::NvidiaDevices => load_nvidia(target),
        }
    }
}

pub fn load_physical_disks() -> Result<Vec<InventoryEntry>, NativeToolInventoryError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        Err(NativeToolInventoryError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        Ok(format_physical_disks(
            &super::quick_partition::get_physical_disks(),
        ))
    }
}

pub fn load_windows_targets(
    partitions: &[super::disk::Partition],
    include_current: bool,
) -> Result<Vec<InventoryEntry>, NativeToolInventoryError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (partitions, include_current);
        Err(NativeToolInventoryError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let mut offline = version_detect::get_windows_partition_infos(partitions)
            .into_iter()
            .map(|partition| InventoryEntry {
                value: partition.letter.clone(),
                label: format!(
                    "{} [{}] [{}]",
                    partition.letter, partition.windows_version, partition.architecture
                ),
                disk_fingerprint: None,
            })
            .collect::<Vec<_>>();
        let system_drive = std::env::var("SystemDrive").ok();
        if include_current {
            remove_current_system_drive(&mut offline, system_drive.as_deref(), |entry| {
                entry.value.as_str()
            });
        } else {
            prefer_system_drive(&mut offline, system_drive.as_deref(), |entry| {
                entry.value.as_str()
            });
        }

        let mut result = Vec::with_capacity(offline.len() + usize::from(include_current));
        if include_current {
            result.push(InventoryEntry {
                value: "当前系统".into(),
                label: crate::tr!("当前系统"),
                disk_fingerprint: None,
            });
        }
        result.extend(offline);
        Ok(result)
    }
}

pub fn load_boot_repair_targets(
    partitions: &[super::disk::Partition],
) -> Result<Vec<super::native_boot_repair::BootRepairTarget>, NativeToolInventoryError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = partitions;
        Err(NativeToolInventoryError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let mut targets = version_detect::get_windows_partition_infos(partitions)
            .into_iter()
            .map(|partition| super::native_boot_repair::BootRepairTarget {
                partition: partition.letter,
                windows_version: partition.windows_version,
                architecture: partition.architecture,
            })
            .collect::<Vec<_>>();
        prefer_current_system_drive(&mut targets, |target| target.partition.as_str());
        Ok(targets)
    }
}

fn prefer_current_system_drive<T>(items: &mut Vec<T>, value: impl for<'a> Fn(&'a T) -> &'a str) {
    let system_drive = std::env::var("SystemDrive").ok();
    prefer_system_drive(items, system_drive.as_deref(), value);
}

fn prefer_system_drive<T>(
    items: &mut Vec<T>,
    system_drive: Option<&str>,
    value: impl for<'a> Fn(&'a T) -> &'a str,
) {
    let Some(system_drive) = normalized_drive(system_drive) else {
        return;
    };
    let Some(index) = items.iter().position(|item| {
        normalized_drive(Some(value(item)))
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&system_drive))
    }) else {
        return;
    };
    if index != 0 {
        let preferred = items.remove(index);
        items.insert(0, preferred);
    }
}

fn remove_current_system_drive<T>(
    items: &mut Vec<T>,
    system_drive: Option<&str>,
    value: impl for<'a> Fn(&'a T) -> &'a str,
) {
    let Some(system_drive) = normalized_drive(system_drive) else {
        return;
    };
    items.retain(|item| {
        !normalized_drive(Some(value(item)))
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&system_drive))
    });
}

fn normalized_drive(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .map(|value| value.trim_end_matches(['\\', '/']))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn format_physical_disks(disks: &[super::quick_partition::PhysicalDisk]) -> Vec<InventoryEntry> {
    disks
        .iter()
        .map(|disk| InventoryEntry {
            value: disk.disk_number.to_string(),
            label: disk.display_name(),
            disk_fingerprint: Some(super::native_quick_partition::DiskFingerprint::from(disk)),
        })
        .collect()
}

#[cfg(not(feature = "non-elevated-tests"))]
fn current_target(target: &str) -> bool {
    target == "当前系统"
        || target.eq_ignore_ascii_case("__CURRENT__")
        || target.eq_ignore_ascii_case("__ONLINE__")
}

#[cfg(not(feature = "non-elevated-tests"))]
fn load_accounts(target: &str) -> Result<Vec<InventoryEntry>, NativeToolInventoryError> {
    let accounts = if current_target(target) {
        let request = CommandRequest::new("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-LocalUser | ForEach-Object { \"$($_.Name)|$($_.Enabled)\" }",
        ]);
        let outcome = SystemCommandExecutor
            .execute(&request)
            .map_err(|error| NativeToolInventoryError::Read(error.to_string()))?;
        if !outcome.succeeded() {
            return Err(NativeToolInventoryError::Read(
                String::from_utf8_lossy(outcome.stderr()).trim().to_string(),
            ));
        }
        String::from_utf8_lossy(outcome.stdout())
            .lines()
            .filter_map(|line| {
                let (name, enabled) = line.trim().split_once('|')?;
                (!name.is_empty()).then(|| (name.to_string(), enabled.eq_ignore_ascii_case("true")))
            })
            .collect::<Vec<_>>()
    } else {
        validate_drive(target)?;
        lr_core::sam::list_accounts(target)
            .map_err(|error| NativeToolInventoryError::Read(error.to_string()))?
            .into_iter()
            .map(|account| (account.username, !account.disabled))
            .collect()
    };
    Ok(accounts
        .into_iter()
        .map(|(name, enabled)| InventoryEntry {
            value: name.clone(),
            label: format!(
                "{} ({})",
                name,
                if enabled {
                    crate::tr!("已启用")
                } else {
                    crate::tr!("已禁用")
                }
            ),
            disk_fingerprint: None,
        })
        .collect())
}

#[cfg(not(feature = "non-elevated-tests"))]
fn load_appx(target: &str) -> Result<Vec<InventoryEntry>, NativeToolInventoryError> {
    if !current_target(target) {
        validate_drive(target)?;
        return super::native_appx::offline_inventory(target)
            .map_err(|error| NativeToolInventoryError::Read(error.to_string()))
            .map(|packages| {
                packages
                    .into_iter()
                    .map(|package| InventoryEntry {
                        value: package.package_name,
                        label: package.display_name,
                        disk_fingerprint: None,
                    })
                    .collect()
            });
    }
    Ok(super::native_appx_legacy::current_inventory()
        .into_iter()
        .map(|package| InventoryEntry {
            value: package.package_name,
            label: package.display_name,
            disk_fingerprint: None,
        })
        .collect())
}

#[cfg(not(feature = "non-elevated-tests"))]
fn load_nvidia(target: &str) -> Result<Vec<InventoryEntry>, NativeToolInventoryError> {
    if !current_target(target) {
        validate_drive(target)?;
        return Ok(vec![InventoryEntry {
            value: format!("{target}:all-nvidia-components"),
            label: crate::tr!("所选离线系统中的全部 NVIDIA 驱动组件"),
            disk_fingerprint: None,
        }]);
    }
    super::nvidia_driver::enumerate_gpu_devices()
        .map_err(|error| NativeToolInventoryError::Read(error.to_string()))
        .map(|devices| {
            devices
                .into_iter()
                .filter(|device| device.is_nvidia)
                .map(|device| InventoryEntry {
                    value: if device.instance_id.is_empty() {
                        device.hardware_id.clone()
                    } else {
                        device.instance_id
                    },
                    label: super::nvidia_driver::beautify_gpu_name(
                        if device.friendly_name.is_empty() {
                            &device.name
                        } else {
                            &device.friendly_name
                        },
                    ),
                    disk_fingerprint: None,
                })
                .collect()
        })
}

#[cfg(not(feature = "non-elevated-tests"))]
fn validate_drive(target: &str) -> Result<(), NativeToolInventoryError> {
    if matches!(target.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) {
        Ok(())
    } else {
        Err(NativeToolInventoryError::InvalidTarget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_disk_labels_keep_typed_disk_numbers() {
        let disks = [super::super::quick_partition::PhysicalDisk {
            disk_number: 7,
            size_bytes: 512 * 1024 * 1024 * 1024,
            model: "NVMe Test".into(),
            partition_style: super::super::disk::PartitionStyle::GPT,
            is_initialized: true,
            partitions: Vec::new(),
            unallocated_bytes: 0,
        }];
        let entries = format_physical_disks(&disks);
        assert_eq!(entries[0].value, "7");
        assert!(entries[0].label.contains("NVMe Test"));
        assert!(entries[0].label.contains("512.0 GB"));
    }

    #[test]
    fn online_current_system_removes_the_same_offline_drive_only() {
        let mut targets = vec![
            InventoryEntry {
                value: "D:".into(),
                label: "D: [Windows 10] [x64]".into(),
                disk_fingerprint: None,
            },
            InventoryEntry {
                value: "c:\\".into(),
                label: "C: [Windows 11] [x64]".into(),
                disk_fingerprint: None,
            },
            InventoryEntry {
                value: "E:".into(),
                label: "E: [Windows 11] [x64]".into(),
                disk_fingerprint: None,
            },
        ];

        remove_current_system_drive(&mut targets, Some(" C: "), |entry| entry.value.as_str());

        assert_eq!(
            targets
                .iter()
                .map(|entry| entry.value.as_str())
                .collect::<Vec<_>>(),
            vec!["D:", "E:"]
        );
    }

    #[test]
    fn offline_only_inventory_still_prefers_the_current_drive() {
        let mut targets = vec!["D:".to_owned(), "c:\\".to_owned(), "E:".to_owned()];
        prefer_system_drive(&mut targets, Some("C:"), String::as_str);
        assert_eq!(targets, vec!["c:\\", "D:", "E:"]);
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_denies_before_host_inventory() {
        assert_eq!(
            load_dynamic(DynamicInventoryKind::ResetPasswordAccounts, "当前系统"),
            Err(NativeToolInventoryError::DevelopmentBuildDenied)
        );
        assert_eq!(
            load_physical_disks(),
            Err(NativeToolInventoryError::DevelopmentBuildDenied)
        );
    }
}
