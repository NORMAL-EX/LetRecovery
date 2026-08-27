//! Normal-Windows planning for full-disk reinstall and dual boot.
//!
//! This module performs read-only inventory and builds a shared typed intent. It does not mutate a
//! disk; dual-boot shrink/create is executed later by the explicit pre-reboot transaction.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use lr_core::custom_install::{
    plan_full_disk_layout, validate_dual_boot_plan, validate_full_disk_plan, CustomInstallPlan,
    DualBootPlan, FullDiskRole, FullDiskSelection, ImageSpaceRequirement, PlannedPartitionRole,
    RepartitionAllDisksPlan, RequestedPartitionStyle, GIB,
};
use lr_core::data_staging::StorageAttachment;

use super::disk::Partition;

#[derive(Clone, Debug)]
pub struct CapturedDisk {
    pub disk_number: u32,
    pub size_bytes: u64,
    pub model: String,
    pub style: RequestedPartitionStyle,
    pub attachment: StorageAttachment,
    /// One current mounted basic-data volume suitable for publishing the per-disk random locator.
    pub marker_letter: Option<char>,
}

impl CapturedDisk {
    pub fn display_name(&self) -> String {
        let size = self.size_bytes as f64 / GIB as f64;
        if self.model.trim().is_empty() {
            format!("磁盘 {} ({size:.1} GB)", self.disk_number)
        } else {
            format!(
                "磁盘 {} - {} ({size:.1} GB)",
                self.disk_number,
                self.model.trim()
            )
        }
    }
}

/// Capture only SetupAPI-present disk interfaces. This never probes guessed PhysicalDrive numbers.
pub fn capture_disk_inventory() -> Result<Vec<CapturedDisk>> {
    let disks = super::quick_partition::get_present_physical_disk_inventory()?;
    if disks.is_empty() {
        bail!("没有枚举到当前存在的物理磁盘接口");
    }
    let mut captured = Vec::with_capacity(disks.len());
    for snapshot in disks {
        let disk = snapshot.disk;
        let style = match disk.partition_style {
            super::disk::PartitionStyle::GPT => RequestedPartitionStyle::Gpt,
            super::disk::PartitionStyle::MBR => RequestedPartitionStyle::Mbr,
            super::disk::PartitionStyle::Unknown => {
                // A RAW/uninitialized unrelated disk is valid inventory noise. It cannot publish a
                // random locator until it has a mounted volume, so it is not selectable in this
                // handoff; do not let its presence block eligible disks. GPT is only a diagnostic
                // default here and is never used unless a future UI explicitly makes it eligible.
                RequestedPartitionStyle::Gpt
            }
        };
        let marker_letter = disk
            .partitions
            .iter()
            .find(|partition| {
                partition.drive_letter.is_some()
                    && !partition.is_esp
                    && !partition.is_msr
                    && !partition.is_recovery
            })
            .and_then(|partition| partition.drive_letter)
            .map(|letter| letter.to_ascii_uppercase());
        captured.push(CapturedDisk {
            disk_number: disk.disk_number,
            size_bytes: disk.size_bytes,
            model: disk.model,
            style,
            attachment: snapshot.attachment,
            marker_letter,
        });
    }
    captured.sort_by_key(|disk| disk.disk_number);
    Ok(captured)
}

/// Resolve the current-session publication paths for a confirmed full-disk plan. Disk numbers are
/// used only here, before reboot, to place each random locator on the disk the UI displayed.
pub fn full_disk_locator_paths(plan: &RepartitionAllDisksPlan) -> Result<Vec<(PathBuf, String)>> {
    let inventory = capture_disk_inventory()?;
    let mut paths = Vec::with_capacity(plan.disks.len());
    for selection in &plan.disks {
        let current = inventory
            .iter()
            .find(|disk| disk.disk_number == selection.diagnostic_disk_number)
            .with_context(|| {
                format!(
                    "confirmed disk {} is no longer present through SetupAPI",
                    selection.diagnostic_disk_number
                )
            })?;
        if current.attachment != StorageAttachment::Internal || current.style != selection.style {
            bail!(
                "confirmed disk {} no longer has the displayed internal-disk properties",
                selection.diagnostic_disk_number
            );
        }
        let letter = current.marker_letter.with_context(|| {
            format!(
                "confirmed disk {} no longer has a mounted basic-data volume for publishing its locator",
                selection.diagnostic_disk_number
            )
        })?;
        paths.push((
            PathBuf::from(format!(
                "{}:\\{}",
                letter,
                lr_core::install_handoff::FULL_DISK_MARKER_NAME
            )),
            selection.locator_token.clone(),
        ));
    }
    Ok(paths)
}

/// Build a plan only from disk numbers the confirmation UI displayed and the user accepted.
pub fn build_full_disk_plan(
    inventory: &[CapturedDisk],
    confirmed_disks: &[u32],
    windows_disk_number: u32,
    image: ImageSpaceRequirement,
) -> Result<CustomInstallPlan> {
    if confirmed_disks.is_empty() {
        bail!("没有选择要清空的内部硬盘");
    }
    let selected = confirmed_disks.iter().copied().collect::<BTreeSet<_>>();
    if selected.len() != confirmed_disks.len() {
        bail!("全盘重装选择中包含重复磁盘");
    }
    if !selected.contains(&windows_disk_number) {
        bail!("Windows 目标磁盘不在已确认清空的磁盘列表中");
    }

    let mut plans = Vec::with_capacity(selected.len());
    // Keep the selected image minimum in the authenticated handoff even when the current disk
    // layout gives Windows all remaining space. `plan_full_disk_layout` already expands Windows
    // into an otherwise too-small optional-data tail. Replacing the minimum with the historical
    // zero/all-remaining sentinel would lose the only lower bound when same-disk staging moves the
    // usable end before PE rebuilds the layout.
    let windows_partition_bytes = image.windows_partition_bytes;
    for disk_number in selected {
        let disk = inventory
            .iter()
            .find(|disk| disk.disk_number == disk_number)
            .with_context(|| format!("已确认磁盘 {disk_number} 不在当前 SetupAPI 清单中"))?;
        if disk.attachment != StorageAttachment::Internal {
            bail!("磁盘 {disk_number} 未被确认是电脑内置硬盘，不能加入清盘计划");
        }
        if disk.marker_letter.is_none() {
            bail!("磁盘 {disk_number} 没有可写入本次随机定位标志的现有数据卷");
        }
        let token = lr_core::handoff_auth::generate_locator_token()
            .context("生成全盘磁盘定位随机值失败")?;
        let role = if disk_number == windows_disk_number {
            // Reuse the exact shared layout policy instead of subtracting a second guessed
            // "infrastructure reserve". The latter used to reserve 1 GiB for a GPT layout whose
            // actual planned fixed partitions are much smaller, causing a false capacity failure.
            let candidate = plan_full_disk_layout(
                disk.style,
                FullDiskRole::Windows,
                disk.size_bytes,
                image.windows_partition_bytes,
            )
            .map_err(|_| {
                anyhow!(
                    "磁盘 {disk_number} 可用容量不足：所选镜像至少需要 {:.1} GB Windows 分区",
                    image.windows_partition_bytes as f64 / GIB as f64
                )
            })?;
            let windows = candidate
                .iter()
                .find(|partition| partition.role == PlannedPartitionRole::Windows)
                .context("共享全盘布局没有生成 Windows 分区")?;
            if windows.length_bytes < image.windows_partition_bytes {
                bail!(
                    "磁盘 {disk_number} 可用容量不足：所选镜像至少需要 {:.1} GB Windows 分区",
                    image.windows_partition_bytes as f64 / GIB as f64
                );
            }
            FullDiskRole::Windows
        } else {
            FullDiskRole::Data
        };
        plans.push(FullDiskSelection {
            diagnostic_disk_number: disk.disk_number,
            locator_token: token.as_str().to_owned(),
            style: disk.style,
            role,
        });
    }
    let plan = RepartitionAllDisksPlan {
        disks: plans,
        windows_partition_bytes,
        preserved_staging: None,
    };
    validate_full_disk_plan(&plan).map_err(|error| anyhow!(error))?;
    Ok(CustomInstallPlan::RepartitionAllDisks(plan))
}

/// Build the desired dual-boot geometry. The returned target/data extents are filled by the
/// normal-Windows precreation transaction before the plan is published to PE.
pub fn build_dual_boot_request(
    source: &Partition,
    requested_windows_bytes: u64,
    optional_data_bytes: u64,
) -> Result<DualBootPlan> {
    let letter = source
        .letter
        .chars()
        .next()
        .filter(|letter| letter.is_ascii_alphabetic())
        .context("双系统源卷没有有效盘符")?
        .to_ascii_uppercase();
    let source_offset = source
        .partition_offset_bytes
        .context("双系统源卷缺少起始偏移")?;
    let source_length = source
        .partition_size_bytes
        .context("双系统源卷缺少精确长度")?;
    if requested_windows_bytes == 0 {
        bail!("双系统 Windows 分区大小必须大于零");
    }
    // These values are capacity requirements, not raw-I/O alignment requests. Preserve the exact
    // image-derived/provider-facing byte counts; the VDS provider chooses the actual geometry.
    let windows_bytes = requested_windows_bytes;
    let data_bytes = optional_data_bytes;
    let total = windows_bytes
        .checked_add(data_bytes)
        .context("双系统分区总大小溢出")?;
    // QueryMaxReclaimableBytes is advisory, not a write-capability contract. The normal-Windows
    // preparation transaction executes the real VDS Shrink before reboot and accepts the plan
    // only when the current extent readback proves at least this minimum was reclaimed.
    if total >= source_length {
        bail!("双系统分区会占用整个源卷");
    }
    let current_source_length = source_length - total;
    let target_offset = source_offset
        .checked_add(current_source_length)
        .context("双系统目标偏移溢出")?;
    let data_offset = (data_bytes != 0)
        .then(|| target_offset.checked_add(windows_bytes))
        .flatten();
    if data_bytes != 0 && data_offset.is_none() {
        bail!("双系统数据卷偏移溢出");
    }
    let plan = DualBootPlan {
        source_drive_letter: letter,
        source_offset_bytes: source_offset,
        source_length_before_bytes: source_length,
        source_length_after_bytes: current_source_length,
        target_offset_bytes: target_offset,
        target_length_bytes: windows_bytes,
        data_offset_bytes: data_offset,
        data_length_bytes: data_bytes,
    };
    validate_dual_boot_plan(&plan).map_err(|error| anyhow!(error))?;
    Ok(plan)
}

/// Add an HRESULT-preserving diagnostic only after the authoritative VDS Shrink failed.
pub fn annotate_shrink_error(error: &lr_core::windows_storage::StorageError) -> String {
    let message = error.to_string();
    let hresult = parse_hresult(&message);
    let Some(hresult) = hresult else {
        return message;
    };
    let availability = lr_core::service_diagnostic::query_service_availability("defragsvc");
    match lr_core::service_diagnostic::explain_shrink_not_supported(hresult, availability) {
        Some(hint) => format!("{message}；{hint}"),
        None => message,
    }
}

fn parse_hresult(message: &str) -> Option<i32> {
    let index = message.find("0x")?;
    let hex = message[index + 2..]
        .chars()
        .take_while(|character| character.is_ascii_hexdigit())
        .collect::<String>();
    (hex.len() == 8)
        .then(|| u32::from_str_radix(&hex, 16).ok().map(|value| value as i32))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(number: u32, size: u64, marker: char) -> CapturedDisk {
        CapturedDisk {
            disk_number: number,
            size_bytes: size,
            model: format!("Test {number}"),
            style: RequestedPartitionStyle::Gpt,
            attachment: StorageAttachment::Internal,
            marker_letter: Some(marker),
        }
    }

    #[test]
    fn full_disk_plan_has_one_windows_target_and_only_confirmed_disks() {
        let inventory = [captured(0, 512 * GIB, 'C'), captured(2, 1024 * GIB, 'D')];
        let plan = build_full_disk_plan(
            &inventory,
            &[2, 0],
            0,
            ImageSpaceRequirement {
                windows_partition_bytes: 64 * GIB,
                expanded_bytes: Some(62 * GIB),
                source: lr_core::custom_install::ImageSpaceSource::WimMetadata,
            },
        )
        .unwrap();
        let CustomInstallPlan::RepartitionAllDisks(plan) = plan else {
            panic!("wrong mode")
        };
        assert_eq!(plan.disks.len(), 2);
        assert_eq!(
            plan.disks
                .iter()
                .filter(|disk| disk.role == FullDiskRole::Windows)
                .count(),
            1
        );
    }

    #[test]
    fn full_disk_plan_does_not_implicitly_select_other_internal_disks() {
        let inventory = [captured(0, 512 * GIB, 'C'), captured(1, 1024 * GIB, 'D')];
        let plan = build_full_disk_plan(
            &inventory,
            &[0],
            0,
            ImageSpaceRequirement {
                windows_partition_bytes: 64 * GIB,
                expanded_bytes: Some(62 * GIB),
                source: lr_core::custom_install::ImageSpaceSource::WimMetadata,
            },
        )
        .unwrap();
        let CustomInstallPlan::RepartitionAllDisks(plan) = plan else {
            panic!("wrong mode")
        };
        assert_eq!(plan.disks.len(), 1);
        assert_eq!(plan.disks[0].diagnostic_disk_number, 0);
    }

    #[test]
    fn constrained_disk_gives_all_remaining_space_to_windows() {
        let plan = build_full_disk_plan(
            &[captured(0, 68 * GIB, 'C')],
            &[0],
            0,
            ImageSpaceRequirement {
                windows_partition_bytes: 64 * GIB,
                expanded_bytes: Some(62 * GIB),
                source: lr_core::custom_install::ImageSpaceSource::WimMetadata,
            },
        )
        .unwrap();
        let CustomInstallPlan::RepartitionAllDisks(plan) = plan else {
            unreachable!()
        };
        assert_eq!(plan.windows_partition_bytes, 64 * GIB);
        assert!(lr_core::custom_install::plan_full_disk_layout(
            RequestedPartitionStyle::Gpt,
            FullDiskRole::Windows,
            60 * GIB,
            plan.windows_partition_bytes,
        )
        .is_err());
    }

    #[test]
    fn full_disk_capacity_uses_the_shared_real_layout_not_a_guessed_one_gib_reserve() {
        let image_bytes = 64 * GIB;
        // Keep this boundary tied to the shared GPT layout instead of baking in the former
        // 300-MiB ESP + 16-MiB MSR assumption. Windows 7 compatibility requires a 128-MiB MSR.
        let disk_bytes = image_bytes
            + lr_core::custom_install::MIB
            + lr_core::custom_install::ESP_4KN_MINIMUM_BYTES
            + lr_core::custom_install::MSR_WINDOWS_7_MINIMUM_BYTES;
        let plan = build_full_disk_plan(
            &[captured(0, disk_bytes, 'C')],
            &[0],
            0,
            ImageSpaceRequirement {
                windows_partition_bytes: image_bytes,
                expanded_bytes: Some(image_bytes - 2 * GIB),
                source: lr_core::custom_install::ImageSpaceSource::WimMetadata,
            },
        )
        .unwrap();
        let CustomInstallPlan::RepartitionAllDisks(plan) = plan else {
            unreachable!()
        };
        assert_eq!(plan.windows_partition_bytes, image_bytes);
    }

    #[test]
    fn dual_boot_query_estimate_is_not_a_pre_shrink_failure_gate() {
        let source = Partition {
            letter: "C:".to_owned(),
            total_size_mb: 256 * 1024,
            free_size_mb: 128 * 1024,
            free_size_bytes: 128 * GIB,
            label: "OS".to_owned(),
            is_system_partition: true,
            has_windows: true,
            partition_style: super::super::disk::PartitionStyle::GPT,
            disk_number: Some(0),
            partition_number: Some(3),
            disk_size_bytes: Some(512 * GIB),
            partition_offset_bytes: Some(1024 * 1024 + 512),
            partition_size_bytes: Some(256 * GIB + 4096),
            partition_kind: Some(lr_core::windows_storage::PartitionKind::BasicData),
            install_target_eligible: true,
            storage_media: lr_core::data_staging::StorageMedia::SolidState,
            stable_identity: None,
            bitlocker_status: crate::core::bitlocker::VolumeStatus::NotEncrypted,
        };
        let requested = 80 * GIB + 4096;
        let plan = build_dual_boot_request(&source, requested, 0).unwrap();
        assert_eq!(plan.target_length_bytes, requested);
        assert_eq!(plan.source_offset_bytes, 1024 * 1024 + 512);
    }

    #[test]
    fn dual_boot_accepts_the_exact_twenty_gib_image_derived_default() {
        let source = Partition {
            letter: "C:".to_owned(),
            total_size_mb: 256 * 1024,
            free_size_mb: 128 * 1024,
            free_size_bytes: 128 * GIB,
            label: "OS".to_owned(),
            is_system_partition: true,
            has_windows: true,
            partition_style: super::super::disk::PartitionStyle::GPT,
            disk_number: Some(0),
            partition_number: Some(3),
            disk_size_bytes: Some(512 * GIB),
            partition_offset_bytes: Some(1024 * 1024 + 512),
            partition_size_bytes: Some(256 * GIB + 4096),
            partition_kind: Some(lr_core::windows_storage::PartitionKind::BasicData),
            install_target_eligible: true,
            storage_media: lr_core::data_staging::StorageMedia::SolidState,
            stable_identity: None,
            bitlocker_status: crate::core::bitlocker::VolumeStatus::NotEncrypted,
        };
        let requirement = lr_core::custom_install::image_space_requirement(18 * GIB, 0);
        assert_eq!(requirement.windows_partition_bytes, 20 * GIB);
        let plan = build_dual_boot_request(&source, requirement.windows_partition_bytes, 0)
            .expect("the image-derived 20 GiB value must not acquire a second fixed floor");
        assert_eq!(plan.target_length_bytes, 20 * GIB);
        let encoded = CustomInstallPlan::DualBoot(plan.clone()).to_json().unwrap();
        assert_eq!(
            CustomInstallPlan::from_json(&encoded).unwrap(),
            CustomInstallPlan::DualBoot(plan)
        );
    }

    #[test]
    fn hresult_parser_preserves_the_real_vds_code() {
        assert_eq!(
            parse_hresult("shrink failed: HRESULT 0x80070032"),
            Some(0x8007_0032_u32 as i32)
        );
        assert_eq!(parse_hresult("other failure"), None);
    }
}
