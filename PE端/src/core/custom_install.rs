//! PE execution boundary for authenticated full-disk and dual-boot installation plans.
//!
//! Cross-reboot selection is random-marker based. Disk numbers below are obtained from the marker
//! volume's current `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` result and are never compared with the
//! normal-endpoint diagnostic disk numbers.

use anyhow::{bail, Context, Result};

use lr_core::custom_install::{
    plan_full_disk_layout, validate_existing_staging_extent, CustomInstallPlan, FullDiskRole,
    PlannedPartition, PlannedPartitionRole, RepartitionAllDisksPlan, RequestedPartitionStyle,
    BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES, ESP_4KN_MINIMUM_BYTES, ESP_512_MINIMUM_BYTES,
    MIN_USEFUL_DATA_BYTES, MSR_WINDOWS_7_MINIMUM_BYTES,
};
use lr_core::windows_storage::{
    CreatePartitionRequest, DiskStyle, FileSystem, FreeExtent, PartitionKind, VolumeIdentity,
};

use super::config::FullDiskExecutionTarget;

pub struct PreparedFullDiskInstall {
    plan: RepartitionAllDisksPlan,
    disks: Vec<PreparedDisk>,
}

struct PreparedDisk {
    locator_token: String,
    role: FullDiskRole,
    current_disk_number: u32,
    diagnostic_disk_number: u32,
    layout: Vec<PlannedPartition>,
    usable_end_bytes: u64,
    preserves_staging: bool,
}

pub struct PreparedInstallTarget {
    pub partition: String,
    pub identity: VolumeIdentity,
    pub staging_cleanup: Option<FullDiskStagingCleanup>,
}

/// Move-only post-install authority for deleting the preserved same-disk staging extent.
///
/// This intentionally contains only the two current extents that participate in the topology
/// change. Historical disk numbers, GUIDs, labels, capacities and layout fingerprints are not
/// authorization inputs.
pub struct FullDiskStagingCleanup {
    disk_number: u32,
    staging_offset_bytes: u64,
    staging_length_bytes: u64,
    recipient_letter: char,
    recipient: VolumeIdentity,
}

fn staging_reclaim_length(
    recipient_offset: u64,
    recipient_length: u64,
    staging_offset: u64,
    staging_length: u64,
) -> Result<u64> {
    let recipient_end = recipient_offset
        .checked_add(recipient_length)
        .context("full-disk staging recipient end overflows")?;
    if recipient_end > staging_offset || staging_length == 0 {
        bail!("the preserved staging extent overlaps or precedes its recipient volume");
    }
    staging_offset
        .checked_add(staging_length)
        .and_then(|staging_end| staging_end.checked_sub(recipient_end))
        .context("full-disk staging reclaim length overflows")
}

/// Keep the mounted ordinary volume that ends closest to the preserved staging extent.
///
/// GPT infrastructure partitions such as ESP and MSR intentionally have no DOS access path. They
/// are valid layout members, but can never receive the later filesystem extend, so they are skipped
/// rather than being misreported as a missing recipient. The caller still fails after the complete
/// layout if no mounted ordinary volume was observed at all.
fn consider_staging_cleanup_recipient(
    current: &mut Option<FullDiskStagingCleanup>,
    disk_number: u32,
    staging_offset_bytes: u64,
    staging_length_bytes: u64,
    created_offset_bytes: u64,
    created_length_bytes: u64,
    created_identity: Option<(char, VolumeIdentity)>,
) -> Result<()> {
    let created_end = created_offset_bytes
        .checked_add(created_length_bytes)
        .context("created partition end overflows")?;
    if created_end > staging_offset_bytes {
        return Ok(());
    }
    let Some((recipient_letter, recipient)) = created_identity else {
        return Ok(());
    };
    let replace = match current.as_ref() {
        None => true,
        Some(existing) => {
            existing
                .recipient
                .offset_bytes
                .checked_add(existing.recipient.extent_length_bytes)
                .context("current staging recipient end overflows")?
                < created_end
        }
    };
    if replace {
        *current = Some(FullDiskStagingCleanup {
            disk_number,
            staging_offset_bytes,
            staging_length_bytes,
            recipient_letter,
            recipient,
        });
    }
    Ok(())
}

fn ensure_rollback_source_unchanged(
    expected: VolumeIdentity,
    current: VolumeIdentity,
) -> Result<()> {
    if !lr_core::windows_storage::same_volume_identity(expected, current) {
        bail!("dual-boot rollback source changed while deleting the task-owned tail volumes");
    }
    Ok(())
}

fn select_current_free_extent(
    extents: &[FreeExtent],
    usable_end_bytes: u64,
    minimum_bytes: u64,
    reserved_tail_bytes: u64,
) -> Result<Option<FreeExtent>> {
    let required_bytes = minimum_bytes
        .checked_add(reserved_tail_bytes)
        .context("current and following partition minimums overflow")?;
    let mut selected = None;
    for extent in extents {
        if extent.length_bytes == 0 {
            continue;
        }
        let end = extent
            .offset_bytes
            .checked_add(extent.length_bytes)
            .context("current full-disk free extent end overflows")?;
        // A provider extent may extend past a preserved staging boundary. Authorize only its
        // exact intersection with the usable range instead of rejecting the legal prefix.
        let authorized_end = end.min(usable_end_bytes);
        let Some(authorized_length) = authorized_end.checked_sub(extent.offset_bytes) else {
            continue;
        };
        if authorized_length < required_bytes {
            continue;
        }
        let candidate = FreeExtent {
            offset_bytes: extent.offset_bytes,
            // Keep enough provider-reported space outside this operation's hard envelope for all
            // later boot-critical partitions. Provider geometry may differ from the request, but
            // it cannot consume capacity already required by a later partition.
            length_bytes: authorized_length - reserved_tail_bytes,
        };
        if selected.is_none_or(|current: FreeExtent| {
            candidate.length_bytes > current.length_bytes
                || (candidate.length_bytes == current.length_bytes
                    && candidate.offset_bytes > current.offset_bytes)
        }) {
            selected = Some(candidate);
        }
    }
    Ok(selected)
}

fn partition_functional_minimum(
    role: PlannedPartitionRole,
    windows_minimum_bytes: u64,
    logical_sector_bytes: Option<u32>,
) -> Result<u64> {
    match role {
        PlannedPartitionRole::EfiSystem => match logical_sector_bytes {
            // Microsoft distinguishes 512-native/512e from 4K-native by the logical sector
            // exposed to Windows. Physical=4096 with logical=512 is 512e, not 4Kn.
            Some(512) => Ok(ESP_512_MINIMUM_BYTES),
            Some(4096) => Ok(ESP_4KN_MINIMUM_BYTES),
            Some(value) => {
                bail!("unsupported logical sector size {value} bytes for an EFI system partition")
            }
            None => bail!("logical sector geometry is required for an EFI system partition"),
        },
        // Windows 7 requires 128 MiB on GPT disks >=16 GiB. A 16-MiB MSR is sufficient only for
        // newer Windows layouts, so it is not the Windows 7-11 functional minimum.
        PlannedPartitionRole::MicrosoftReserved => Ok(MSR_WINDOWS_7_MINIMUM_BYTES),
        // 100 MiB is Microsoft's boot-only BIOS minimum. LetRecovery also supports BitLocker,
        // whose separate NTFS system volume requirement is approximately 350 MiB.
        PlannedPartitionRole::SystemReserved => Ok(BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES),
        PlannedPartitionRole::Windows => Ok(windows_minimum_bytes),
        // This is the existing product threshold for a useful data volume, not an alignment rule.
        // Optional data is skipped if current provider space falls below it.
        PlannedPartitionRole::Data => Ok(MIN_USEFUL_DATA_BYTES),
    }
}

fn following_required_minimum(
    partitions: &[PlannedPartition],
    windows_minimum_bytes: u64,
    logical_sector_bytes: Option<u32>,
    data_is_required: bool,
) -> Result<u64> {
    partitions
        .iter()
        // Data after the Windows partition is optional and must not reduce the image-derived
        // Windows minimum. On a selected data-only disk it is the core result and is reserved.
        .filter(|partition| data_is_required || partition.role != PlannedPartitionRole::Data)
        .try_fold(0_u64, |total, partition| {
            total
                .checked_add(partition_functional_minimum(
                    partition.role,
                    windows_minimum_bytes,
                    logical_sector_bytes,
                )?)
                .context("following partition functional minimums overflow")
        })
}

fn validate_preserved_staging_bounds(
    staging: &lr_core::custom_install::PreservedStagingExtent,
    disk_size_bytes: u64,
    staging_disk_number: u32,
    original_target: VolumeIdentity,
) -> Result<()> {
    if staging_disk_number == original_target.disk_number {
        return validate_existing_staging_extent(
            staging,
            disk_size_bytes,
            original_target.offset_bytes,
            original_target.extent_length_bytes,
        )
        .map_err(anyhow::Error::msg);
    }
    let staging_end = staging
        .offset_bytes
        .checked_add(staging.length_bytes)
        .context("preserved staging extent end overflows")?;
    if staging.offset_bytes == 0 || staging.length_bytes == 0 || staging_end > disk_size_bytes {
        bail!("the preserved staging extent is outside its current disk");
    }
    Ok(())
}

fn rebind_current_preserved_staging(
    plan: &RepartitionAllDisksPlan,
    targets: &[FullDiskExecutionTarget],
    data_identity: VolumeIdentity,
) -> Result<RepartitionAllDisksPlan> {
    let mut current = plan.clone();
    let matching: Vec<_> = targets
        .iter()
        .filter(|target| target.expected.disk_number == data_identity.disk_number)
        .collect();
    match current.preserved_staging.as_mut() {
        Some(staging) => {
            let [target] = matching.as_slice() else {
                bail!(
                    "the preserved staging locator does not resolve uniquely to the current data volume disk"
                );
            };
            if target.locator_token != staging.disk_locator_token {
                bail!("the preserved staging locator names a different current disk");
            }
            if staging.offset_bytes != data_identity.offset_bytes
                || staging.length_bytes != data_identity.extent_length_bytes
            {
                log::info!(
                    "full-disk data marker rebound to current staging extent disk={} offset={} length={}; normal-endpoint diagnostic offset={} length={}",
                    data_identity.disk_number,
                    data_identity.offset_bytes,
                    data_identity.extent_length_bytes,
                    staging.offset_bytes,
                    staging.length_bytes
                );
            }
            staging.offset_bytes = data_identity.offset_bytes;
            staging.length_bytes = data_identity.extent_length_bytes;
        }
        None if !matching.is_empty() => {
            bail!("the data staging volume is on a selected disk but the plan does not preserve it")
        }
        None => {}
    }
    Ok(current)
}

pub fn validate_dual_boot_target(
    plan: &CustomInstallPlan,
    current_target: VolumeIdentity,
    current_data: VolumeIdentity,
) -> Result<()> {
    let CustomInstallPlan::DualBoot(plan) = plan else {
        return Ok(());
    };

    let target_end = current_target
        .offset_bytes
        .checked_add(current_target.extent_length_bytes)
        .context("the current dual-boot target extent overflows")?;
    let data_end = current_data
        .offset_bytes
        .checked_add(current_data.extent_length_bytes)
        .context("the current installation-data extent overflows")?;
    if current_target.extent_length_bytes == 0 || current_data.extent_length_bytes == 0 {
        bail!("the current dual-boot target or installation-data extent is empty");
    }
    if current_target == current_data
        || (current_target.disk_number == current_data.disk_number
            && current_target.offset_bytes < data_end
            && current_data.offset_bytes < target_end)
    {
        bail!("the current dual-boot target overlaps the installation-data extent");
    }

    // The random target marker is the cross-reboot binding. Disk numbers and historical geometry
    // can legitimately change across firmware/provider/WinPE enumeration, so retain them only as
    // diagnostics. Destructive rollback remains a separate transaction and deliberately keeps its
    // exact normal-endpoint extent checks below before deleting any pre-created partition.
    if current_target.offset_bytes != plan.target_offset_bytes
        || current_target.extent_length_bytes != plan.target_length_bytes
    {
        log::info!(
            "dual-boot marker rebound to current extent disk={} offset={} length={}; normal-endpoint diagnostic offset={} length={}",
            current_target.disk_number,
            current_target.offset_bytes,
            current_target.extent_length_bytes,
            plan.target_offset_bytes,
            plan.target_length_bytes
        );
    }
    Ok(())
}

/// Roll back a normal-Windows dual-boot preparation only while the original source system has not
/// been deleted, formatted or handed to an image engine. The current random target marker has
/// already rebound `current_target`; historical drive letters and disk numbers are not used.
pub fn rollback_dual_boot_before_write(
    plan: &CustomInstallPlan,
    current_target: VolumeIdentity,
) -> Result<bool> {
    let CustomInstallPlan::DualBoot(plan) = plan else {
        return Ok(false);
    };
    if current_target.offset_bytes != plan.target_offset_bytes
        || current_target.extent_length_bytes != plan.target_length_bytes
    {
        bail!("dual-boot rollback target differs from the authenticated pre-created extent");
    }
    let disk_number = current_target.disk_number;
    let source_letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
        disk_number,
        plan.source_offset_bytes,
    )?;
    // A volume may legitimately expose more than one DOS drive-letter alias. Every alias in this
    // list already comes from the exact current disk/partition extent, so requiring exactly one
    // adds no wrong-volume protection and can turn a normal mount layout into a rollback failure.
    let source_letter = source_letters
        .first()
        .copied()
        .context("dual-boot rollback source has no current drive-letter access path")?;
    let source_before_cleanup = lr_core::windows_storage::volume_identity(source_letter)?;
    if source_before_cleanup.disk_number != disk_number
        || source_before_cleanup.offset_bytes != plan.source_offset_bytes
        || source_before_cleanup.extent_length_bytes != plan.source_length_after_bytes
    {
        bail!("dual-boot rollback source no longer has the authenticated post-shrink extent");
    }
    let reclaimed = plan
        .source_length_before_bytes
        .checked_sub(plan.source_length_after_bytes)
        .context("dual-boot rollback length underflow")?;
    let tail_offset = plan
        .source_offset_bytes
        .checked_add(plan.source_length_after_bytes)
        .context("dual-boot rollback tail offset overflow")?;
    let partitions = lr_core::windows_storage::partitions(disk_number)?;
    let target_matches = partitions
        .iter()
        .filter(|partition| {
            partition.offset_bytes == plan.target_offset_bytes
                && partition.size_bytes == plan.target_length_bytes
                && partition.kind == PartitionKind::BasicData
        })
        .count();
    let data_matches = plan.data_offset_bytes.map_or(0, |offset| {
        partitions
            .iter()
            .filter(|partition| {
                partition.offset_bytes == offset
                    && partition.size_bytes == plan.data_length_bytes
                    && partition.kind == PartitionKind::BasicData
            })
            .count()
    });
    if target_matches != 1 || data_matches != usize::from(plan.data_offset_bytes.is_some()) {
        bail!("dual-boot rollback target/data extent is absent or ambiguous");
    }
    for partition in &partitions {
        let overlap = lr_core::custom_install::ranges_overlap(
            partition.offset_bytes,
            partition.size_bytes,
            tail_offset,
            reclaimed,
        )
        .map_err(anyhow::Error::msg)?;
        let owned_target = partition.offset_bytes == plan.target_offset_bytes
            && partition.size_bytes == plan.target_length_bytes;
        let owned_data = plan.data_offset_bytes.is_some_and(|offset| {
            partition.offset_bytes == offset && partition.size_bytes == plan.data_length_bytes
        });
        if overlap && !owned_target && !owned_data {
            bail!("dual-boot rollback tail contains a partition not created by this task");
        }
    }
    if let Some(offset) = plan.data_offset_bytes {
        let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
        lr_core::windows_storage::delete_partition_checked(disk_number, offset, false, &snapshot)?;
    }
    let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
    lr_core::windows_storage::delete_partition_checked(
        disk_number,
        plan.target_offset_bytes,
        false,
        &snapshot,
    )?;
    let source_after_deletes = lr_core::windows_storage::volume_identity(source_letter)?;
    ensure_rollback_source_unchanged(source_before_cleanup, source_after_deletes)?;
    lr_core::windows_storage::extend_volume_checked(
        source_letter,
        source_after_deletes,
        reclaimed,
    )?;
    let restored = lr_core::windows_storage::volume_identity(source_letter)?;
    if restored.disk_number != disk_number
        || restored.offset_bytes != plan.source_offset_bytes
        || restored.extent_length_bytes != plan.source_length_before_bytes
    {
        bail!("dual-boot rollback source readback differs from the original extent");
    }
    Ok(true)
}

pub fn preflight_full_disk_install(
    plan: &CustomInstallPlan,
    targets: Vec<FullDiskExecutionTarget>,
    data_identity: VolumeIdentity,
    original_target: VolumeIdentity,
) -> Result<Option<PreparedFullDiskInstall>> {
    let CustomInstallPlan::RepartitionAllDisks(plan) = plan else {
        return Ok(None);
    };
    if targets.len() != plan.disks.len() {
        bail!("authenticated full-disk locator count does not match the plan");
    }
    let current_plan = rebind_current_preserved_staging(plan, &targets, data_identity)?;
    let staging_selection = current_plan.preserved_staging.as_ref();

    let mut seen_disks = std::collections::BTreeSet::new();
    let mut prepared = Vec::with_capacity(targets.len());
    for target in targets {
        let selection = current_plan
            .disks
            .iter()
            .find(|selection| selection.locator_token == target.locator_token)
            .context("full-disk marker is not present in the authenticated plan")?;
        if target.role != selection.role {
            bail!("full-disk marker role differs from the authenticated plan");
        }
        if !seen_disks.insert(target.expected.disk_number) {
            bail!("two selected locators resolved to volumes on the same current disk");
        }
        let initial_layout =
            lr_core::windows_storage::disk_layout_snapshot(target.expected.disk_number)
                .context("read selected disk layout before full-disk write")?;
        let partitions = lr_core::windows_storage::partitions(target.expected.disk_number)
            .context("read selected disk partitions before full-disk write")?;
        let preserves_staging = staging_selection
            .is_some_and(|staging| staging.disk_locator_token == target.locator_token);
        let usable_end = if preserves_staging {
            let staging = staging_selection.expect("presence checked");
            validate_preserved_staging_bounds(
                staging,
                initial_layout.disk_size_bytes,
                data_identity.disk_number,
                original_target,
            )?;
            let exact = partitions
                .iter()
                .filter(|partition| {
                    partition.offset_bytes == staging.offset_bytes
                        && partition.size_bytes == staging.length_bytes
                        && partition.kind == PartitionKind::BasicData
                })
                .count();
            if exact != 1
                || data_identity.disk_number != target.expected.disk_number
                || data_identity.offset_bytes != staging.offset_bytes
                || data_identity.extent_length_bytes != staging.length_bytes
            {
                bail!("the existing staging extent is absent, ambiguous or no longer basic data");
            }
            staging.offset_bytes
        } else {
            initial_layout.disk_size_bytes
        };
        let layout = plan_full_disk_layout(
            selection.style,
            selection.role,
            usable_end,
            current_plan.windows_partition_bytes,
        )
        .map_err(anyhow::Error::msg)?;
        prepared.push(PreparedDisk {
            locator_token: target.locator_token,
            role: target.role,
            current_disk_number: target.expected.disk_number,
            diagnostic_disk_number: target.diagnostic_disk_number,
            layout,
            usable_end_bytes: usable_end,
            preserves_staging,
        });
    }
    Ok(Some(PreparedFullDiskInstall {
        plan: current_plan,
        disks: prepared,
    }))
}

pub fn execute_full_disk_install(
    prepared: PreparedFullDiskInstall,
    released: &[FullDiskExecutionTarget],
) -> Result<PreparedInstallTarget> {
    if released.len() != prepared.disks.len() {
        bail!("full-disk locator release set changed after preflight");
    }
    let mut drive_mask = lr_core::windows_storage::assigned_drive_letter_mask()
        .context("read assigned drive letters")?;
    let mut windows_target = None;
    let mut staging_cleanup: Option<FullDiskStagingCleanup> = None;
    for disk in prepared.disks {
        let released_target = released
            .iter()
            .find(|target| target.locator_token == disk.locator_token)
            .context("full-disk locator was not released for execution")?;
        if released_target.expected.disk_number != disk.current_disk_number {
            bail!("full-disk locator resolved to a different disk after preflight");
        }
        log::info!(
            "[FULL DISK] confirmed diagnostic disk {} currently resolves to disk {} role={:?}",
            disk.diagnostic_disk_number,
            disk.current_disk_number,
            disk.role
        );
        if disk.preserves_staging {
            let staging = prepared
                .plan
                .preserved_staging
                .as_ref()
                .context("preserved staging disappeared from the plan")?;
            let fresh = lr_core::windows_storage::partitions(disk.current_disk_number)
                .context("re-read preserved staging disk before deleting old partitions")?;
            let exact = fresh
                .iter()
                .filter(|partition| {
                    partition.offset_bytes == staging.offset_bytes
                        && partition.size_bytes == staging.length_bytes
                        && partition.kind == PartitionKind::BasicData
                })
                .count();
            if exact != 1 {
                bail!("preserved staging extent changed before the first partition delete");
            }
            for partition in fresh.iter().filter(|partition| {
                partition.offset_bytes != staging.offset_bytes
                    || partition.size_bytes != staging.length_bytes
            }) {
                let snapshot =
                    lr_core::windows_storage::disk_layout_snapshot(disk.current_disk_number)?;
                lr_core::windows_storage::delete_partition_checked(
                    disk.current_disk_number,
                    partition.offset_bytes,
                    true,
                    &snapshot,
                )
                .with_context(|| {
                    format!(
                        "delete old partition {} on selected disk {}",
                        partition.partition_number, disk.current_disk_number
                    )
                })?;
            }
        } else {
            let fresh = lr_core::windows_storage::disk_layout_snapshot(disk.current_disk_number)?;
            lr_core::windows_storage::clean_and_initialize_checked(
                disk.current_disk_number,
                &fresh,
                requested_style(
                    prepared
                        .plan
                        .disks
                        .iter()
                        .find(|value| value.locator_token == disk.locator_token)
                        .expect("validated plan contains locator")
                        .style,
                ),
            )?;
        }

        let layout = disk.layout;
        let partition_count = layout.len();
        let logical_sector_bytes = if layout
            .iter()
            .any(|partition| partition.role == PlannedPartitionRole::EfiSystem)
        {
            // StorageAccessAlignmentProperty is available on Windows 7 and distinguishes 512e
            // from 4Kn without guessing. Query failure is fatal only because FAT32 ESP capacity
            // cannot otherwise be chosen without risking either a false rejection or no boot.
            Some(
                lr_core::windows_storage::physical_disk_sector_geometry(disk.current_disk_number)?
                    .logical_sector_bytes,
            )
        } else {
            None
        };
        for partition_index in 0..partition_count {
            let partition = layout[partition_index];
            let is_final = partition_index + 1 == partition_count;
            let optional_data =
                disk.role == FullDiskRole::Windows && partition.role == PlannedPartitionRole::Data;
            let letter = match partition.role {
                PlannedPartitionRole::Windows | PlannedPartitionRole::Data => {
                    let value = match first_free_letter(drive_mask) {
                        Ok(value) => value,
                        Err(error) if optional_data => {
                            log::warn!(
                                "[FULL DISK] optional data volume skipped because no drive letter is available: {error:#}"
                            );
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    drive_mask |= letter_bit(value);
                    Some(value)
                }
                _ => None,
            };
            let (kind, file_system, label, active) = partition_parameters(partition.role);
            let functional_minimum = partition_functional_minimum(
                partition.role,
                prepared.plan.windows_partition_bytes,
                logical_sector_bytes,
            )?;
            let following_minimum = following_required_minimum(
                &layout[partition_index + 1..],
                prepared.plan.windows_partition_bytes,
                logical_sector_bytes,
                disk.role == FullDiskRole::Data,
            )?;
            let extents = match lr_core::windows_storage::current_free_extents(
                disk.current_disk_number,
            ) {
                Ok(extents) => extents,
                Err(error) if optional_data => {
                    log::warn!(
                        "[FULL DISK] optional data volume skipped because the remaining provider extent could not be queried: {error}"
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let envelope = match select_current_free_extent(
                &extents,
                disk.usable_end_bytes,
                functional_minimum,
                following_minimum,
            )? {
                Some(extent) => extent,
                None if optional_data => {
                    log::warn!(
                        "[FULL DISK] optional data volume skipped because no remaining provider extent is available"
                    );
                    continue;
                }
                None => bail!(
                    "no current provider free extent can satisfy the {:?} volume minimum of {} bytes",
                    partition.role,
                    functional_minimum
                ),
            };
            let snapshot =
                lr_core::windows_storage::disk_layout_snapshot(disk.current_disk_number)?;
            let request = CreatePartitionRequest {
                disk_number: disk.current_disk_number,
                // The final volume consumes the provider's current remaining extent. Earlier
                // volumes keep their requested capacities, while `functional_minimum` remains a
                // separate success condition so legal provider alignment does not become a false
                // exact-size requirement.
                offset_bytes: envelope.offset_bytes,
                size_bytes: if is_final {
                    envelope.length_bytes
                } else {
                    // The layout value is a desired capacity. If earlier provider adjustments
                    // left less room, use the exact current authorized envelope, but never below
                    // this partition's functional minimum or any following required budget.
                    partition.length_bytes.min(envelope.length_bytes)
                },
                kind,
                file_system,
                label: label.to_owned(),
                drive_letter: letter,
                active,
                preserve_gpt_metadata: None,
            };
            let create_result = lr_core::windows_storage::create_partition_checked_in_envelope(
                &request,
                envelope,
                functional_minimum,
                &snapshot,
            );
            let created = match create_result {
                Ok(created) => created,
                Err(error) if optional_data => {
                    log::warn!(
                        "[FULL DISK] optional data volume creation failed and does not invalidate the Windows installation: {error}"
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if partition.role == PlannedPartitionRole::Windows
                && created.size_bytes < prepared.plan.windows_partition_bytes
            {
                bail!(
                    "provider-created Windows volume is {} bytes, below the required {} bytes",
                    created.size_bytes,
                    prepared.plan.windows_partition_bytes
                );
            }
            let created_identity = if let Some(letter) = letter {
                // `create_partition_checked` already binds the requested access path and verifies
                // its post-create canonical extent. Do not add a second whole-machine inventory
                // gate after that checked boundary.
                let identity = VolumeIdentity {
                    disk_number: disk.current_disk_number,
                    offset_bytes: created.offset_bytes,
                    extent_length_bytes: created.size_bytes,
                };
                Some((letter, identity))
            } else {
                None
            };
            if disk.preserves_staging {
                let staging = prepared
                    .plan
                    .preserved_staging
                    .as_ref()
                    .context("preserved staging disappeared from the plan")?;
                consider_staging_cleanup_recipient(
                    &mut staging_cleanup,
                    disk.current_disk_number,
                    staging.offset_bytes,
                    staging.length_bytes,
                    created.offset_bytes,
                    created.size_bytes,
                    created_identity,
                )?;
            }
            if partition.role == PlannedPartitionRole::Windows {
                let (letter, identity) =
                    created_identity.expect("Windows partition receives a drive letter");
                windows_target = Some(PreparedInstallTarget {
                    partition: format!("{letter}:"),
                    identity,
                    staging_cleanup: None,
                });
            }
        }
    }
    if prepared.plan.preserved_staging.is_some() && staging_cleanup.is_none() {
        bail!("full-disk layout created no recipient volume before preserved staging");
    }
    let mut windows_target =
        windows_target.context("full-disk transaction created no Windows target")?;
    windows_target.staging_cleanup = staging_cleanup;
    Ok(windows_target)
}

/// Delete the exact preserved staging partition and extend the exact new adjacent volume.
///
/// This is called only after image application, drivers, unattended setup and boot creation have
/// succeeded. A failure is therefore a post-install warning, never an installation failure.
pub fn cleanup_full_disk_staging(authorization: &FullDiskStagingCleanup) -> Result<VolumeIdentity> {
    let recipient = lr_core::windows_storage::volume_identity(authorization.recipient_letter)
        .context("re-read the full-disk staging recipient volume")?;
    if !lr_core::windows_storage::same_volume_identity(recipient, authorization.recipient) {
        bail!("the full-disk staging recipient volume changed before cleanup");
    }
    let recipient_end = recipient
        .offset_bytes
        .checked_add(recipient.extent_length_bytes)
        .context("full-disk staging recipient end overflows")?;
    if recipient.disk_number != authorization.disk_number
        || recipient_end > authorization.staging_offset_bytes
    {
        bail!("the preserved staging extent overlaps or precedes its recipient volume");
    }
    let staging_end = authorization
        .staging_offset_bytes
        .checked_add(authorization.staging_length_bytes)
        .context("preserved staging extent end overflows")?;
    let reclaim_length = staging_reclaim_length(
        recipient.offset_bytes,
        recipient.extent_length_bytes,
        authorization.staging_offset_bytes,
        authorization.staging_length_bytes,
    )?;
    let layout = lr_core::windows_storage::disk_layout_snapshot(authorization.disk_number)
        .context("read the staging disk before post-install cleanup")?;
    if staging_end > layout.disk_size_bytes {
        bail!("preserved staging extent exceeds the current disk");
    }
    let partitions = lr_core::windows_storage::partitions(authorization.disk_number)
        .context("read current partitions before post-install staging cleanup")?;
    let exact_staging = partitions
        .iter()
        .filter(|partition| {
            partition.offset_bytes == authorization.staging_offset_bytes
                && partition.size_bytes == authorization.staging_length_bytes
                && partition.kind == PartitionKind::BasicData
        })
        .count();
    let exact_recipient = partitions
        .iter()
        .filter(|partition| {
            partition.offset_bytes == recipient.offset_bytes
                && partition.size_bytes == recipient.extent_length_bytes
                && partition.kind == PartitionKind::BasicData
        })
        .count();
    if exact_staging != 1 || exact_recipient != 1 {
        bail!("the recipient or preserved staging extent is absent or ambiguous");
    }
    let gap_length = authorization
        .staging_offset_bytes
        .checked_sub(recipient_end)
        .context("preserved staging gap underflow")?;
    if gap_length != 0 {
        if partitions.iter().any(|partition| {
            lr_core::custom_install::ranges_overlap(
                partition.offset_bytes,
                partition.size_bytes,
                recipient_end,
                gap_length,
            )
            .unwrap_or(true)
        }) {
            bail!("another partition occupies the gap before preserved staging");
        }
        let gap_is_free =
            lr_core::windows_storage::current_free_extents(authorization.disk_number)?
                .iter()
                .any(|extent| {
                    extent.offset_bytes <= recipient_end
                        && extent
                            .offset_bytes
                            .checked_add(extent.length_bytes)
                            .is_some_and(|end| end >= authorization.staging_offset_bytes)
                });
        if !gap_is_free {
            bail!("the provider no longer reports the gap before preserved staging as free");
        }
    }

    lr_core::windows_storage::delete_partition_checked(
        authorization.disk_number,
        authorization.staging_offset_bytes,
        true,
        &layout,
    )
    .context("delete the preserved same-disk staging partition")?;
    lr_core::windows_storage::extend_volume_checked(
        authorization.recipient_letter,
        recipient,
        reclaim_length,
    )
    .context("return preserved staging space to the adjacent volume")?;

    let expected_length = recipient
        .extent_length_bytes
        .checked_add(reclaim_length)
        .context("post-cleanup recipient extent length overflows")?;
    let actual = lr_core::windows_storage::volume_identity(authorization.recipient_letter)
        .context("read back the extended full-disk recipient volume")?;
    if actual.disk_number != recipient.disk_number
        || actual.offset_bytes != recipient.offset_bytes
        || actual.extent_length_bytes != expected_length
    {
        bail!("full-disk staging cleanup finished with an unexpected recipient extent");
    }
    Ok(actual)
}

fn requested_style(style: RequestedPartitionStyle) -> DiskStyle {
    match style {
        RequestedPartitionStyle::Gpt => DiskStyle::Gpt,
        RequestedPartitionStyle::Mbr => DiskStyle::Mbr,
    }
}

fn partition_parameters(
    role: PlannedPartitionRole,
) -> (PartitionKind, Option<FileSystem>, &'static str, bool) {
    match role {
        PlannedPartitionRole::EfiSystem => (
            PartitionKind::EfiSystem,
            Some(FileSystem::Fat32),
            "EFI",
            false,
        ),
        PlannedPartitionRole::MicrosoftReserved => {
            (PartitionKind::MicrosoftReserved, None, "MSR", false)
        }
        PlannedPartitionRole::SystemReserved => (
            PartitionKind::BasicData,
            Some(FileSystem::Ntfs),
            "System Reserved",
            true,
        ),
        PlannedPartitionRole::Windows => (
            PartitionKind::BasicData,
            Some(FileSystem::Ntfs),
            "Windows",
            false,
        ),
        PlannedPartitionRole::Data => (
            PartitionKind::BasicData,
            Some(FileSystem::Ntfs),
            "Data",
            false,
        ),
    }
}

fn letter_bit(letter: char) -> u32 {
    1_u32 << u32::from(letter.to_ascii_uppercase() as u8 - b'A')
}

fn first_free_letter(mask: u32) -> Result<char> {
    (b'C'..=b'Z')
        .map(char::from)
        .find(|letter| mask & letter_bit(*letter) == 0)
        .context("no unused drive letter is available for the full-disk layout")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_boot_target_binding_ignores_cross_boot_disk_guid_number_and_geometry_changes() {
        let plan = CustomInstallPlan::DualBoot(lr_core::custom_install::DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: 1_048_576,
            source_length_before_bytes: 300_000_000_000,
            source_length_after_bytes: 200_000_000_123,
            target_offset_bytes: 200_001_049_211,
            target_length_bytes: 80_000_000_321,
            data_offset_bytes: None,
            data_length_bytes: 0,
        });
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 17,
                offset_bytes: 321_123,
                extent_length_bytes: 79_999_999_777,
            },
            VolumeIdentity {
                disk_number: 23,
                offset_bytes: 8_192,
                extent_length_bytes: 9_000_000_123,
            }
        )
        .is_ok());
    }

    #[test]
    fn dual_boot_target_binding_rejects_empty_overflowing_or_overlapping_current_extents() {
        let plan = CustomInstallPlan::DualBoot(lr_core::custom_install::DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: 1_048_576,
            source_length_before_bytes: 300_000_000_000,
            source_length_after_bytes: 200_000_000_123,
            target_offset_bytes: 200_001_049_211,
            target_length_bytes: 80_000_000_321,
            data_offset_bytes: None,
            data_length_bytes: 0,
        });
        let data = VolumeIdentity {
            disk_number: 9,
            offset_bytes: 10_000,
            extent_length_bytes: 20_000,
        };
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 8,
                offset_bytes: 1,
                extent_length_bytes: 0,
            },
            data,
        )
        .is_err());
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 8,
                offset_bytes: u64::MAX - 10,
                extent_length_bytes: 20,
            },
            data,
        )
        .is_err());
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 9,
                offset_bytes: 25_000,
                extent_length_bytes: 10_000,
            },
            data,
        )
        .is_err());
    }

    #[test]
    fn full_disk_staging_rebinds_to_the_random_data_markers_current_extent() {
        let plan = RepartitionAllDisksPlan {
            disks: vec![lr_core::custom_install::FullDiskSelection {
                diagnostic_disk_number: 1,
                locator_token: "selected-disk-token".into(),
                style: RequestedPartitionStyle::Gpt,
                role: FullDiskRole::Windows,
            }],
            windows_partition_bytes: 80 * lr_core::custom_install::GIB,
            preserved_staging: Some(lr_core::custom_install::PreservedStagingExtent {
                disk_locator_token: "selected-disk-token".into(),
                offset_bytes: 900_000_000_000,
                length_bytes: 10_000_000_000,
            }),
        };
        let targets = vec![FullDiskExecutionTarget {
            locator_token: "selected-disk-token".into(),
            diagnostic_disk_number: 1,
            role: FullDiskRole::Windows,
            partition: r"\\?\Volume{test}\".into(),
            expected: VolumeIdentity {
                disk_number: 17,
                offset_bytes: 4096,
                extent_length_bytes: 123_456_789,
            },
        }];
        let current_data = VolumeIdentity {
            disk_number: 17,
            offset_bytes: 700_000_321,
            extent_length_bytes: 9_999_999_777,
        };
        let rebound = rebind_current_preserved_staging(&plan, &targets, current_data).unwrap();
        let staging = rebound.preserved_staging.unwrap();
        assert_eq!(staging.offset_bytes, current_data.offset_bytes);
        assert_eq!(staging.length_bytes, current_data.extent_length_bytes);
        assert_eq!(staging.disk_locator_token, "selected-disk-token");
    }

    #[test]
    fn full_disk_staging_rebind_rejects_an_unrelated_or_ambiguous_current_disk() {
        let plan = RepartitionAllDisksPlan {
            disks: vec![],
            windows_partition_bytes: 1,
            preserved_staging: Some(lr_core::custom_install::PreservedStagingExtent {
                disk_locator_token: "wanted".into(),
                offset_bytes: 1,
                length_bytes: 1,
            }),
        };
        let target = |token: &str| FullDiskExecutionTarget {
            locator_token: token.into(),
            diagnostic_disk_number: 1,
            role: FullDiskRole::Windows,
            partition: r"\\?\Volume{test}\".into(),
            expected: VolumeIdentity {
                disk_number: 17,
                offset_bytes: 4096,
                extent_length_bytes: 123_456_789,
            },
        };
        let data = VolumeIdentity {
            disk_number: 17,
            offset_bytes: 700_000_321,
            extent_length_bytes: 9_999_999_777,
        };
        assert!(rebind_current_preserved_staging(&plan, &[target("other")], data).is_err());
        assert!(rebind_current_preserved_staging(
            &plan,
            &[target("wanted"), target("wanted")],
            data,
        )
        .is_err());
    }

    #[test]
    fn staging_cleanup_includes_a_legal_non_mib_tail_gap() {
        let recipient_offset = 1_048_576;
        let recipient_length = 60 * 1024 * 1024 * 1024;
        let gap = 63 * 1024;
        let staging_offset = recipient_offset + recipient_length + gap;
        let staging_length = 8 * 1024 * 1024 * 1024 + 512;
        assert_eq!(
            staging_reclaim_length(
                recipient_offset,
                recipient_length,
                staging_offset,
                staging_length,
            )
            .unwrap(),
            gap + staging_length
        );
    }

    #[test]
    fn hidden_gpt_partitions_do_not_block_the_mounted_staging_recipient() {
        let disk_number = 0;
        let staging_offset = 127_307_612_160;
        let staging_length = 10_130_292_736;
        let mut selected = None;

        // ESP and MSR intentionally have no drive letter. The production failure captured on
        // 2026-08-14 stopped on the first of these instead of continuing to the Windows volume.
        consider_staging_cleanup_recipient(
            &mut selected,
            disk_number,
            staging_offset,
            staging_length,
            1_048_576,
            300 * 1024 * 1024,
            None,
        )
        .unwrap();
        consider_staging_cleanup_recipient(
            &mut selected,
            disk_number,
            staging_offset,
            staging_length,
            315_638_271,
            128 * 1024 * 1024,
            None,
        )
        .unwrap();
        assert!(selected.is_none());

        let windows = VolumeIdentity {
            disk_number,
            offset_bytes: 449_855_999,
            extent_length_bytes: 21_126_799_329,
        };
        consider_staging_cleanup_recipient(
            &mut selected,
            disk_number,
            staging_offset,
            staging_length,
            windows.offset_bytes,
            windows.extent_length_bytes,
            Some(('C', windows)),
        )
        .unwrap();

        // A later hidden recovery/infrastructure partition must not erase the valid ordinary
        // recipient already selected.
        consider_staging_cleanup_recipient(
            &mut selected,
            disk_number,
            staging_offset,
            staging_length,
            126_900_000_123,
            300_000_321,
            None,
        )
        .unwrap();
        let selected = selected.unwrap();
        assert_eq!(selected.recipient_letter, 'C');
        assert_eq!(selected.recipient, windows);
        assert_eq!(selected.staging_offset_bytes, staging_offset);
        assert_eq!(selected.staging_length_bytes, staging_length);
    }

    #[test]
    fn later_mounted_volume_replaces_an_earlier_staging_recipient() {
        let mut selected = None;
        let windows = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 449_855_999,
            extent_length_bytes: 21_126_799_329,
        };
        let data = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 21_576_655_777,
            extent_length_bytes: 90_000_000_123,
        };
        for (letter, identity) in [('C', windows), ('D', data)] {
            consider_staging_cleanup_recipient(
                &mut selected,
                3,
                127_307_612_160,
                10_130_292_736,
                identity.offset_bytes,
                identity.extent_length_bytes,
                Some((letter, identity)),
            )
            .unwrap();
        }
        let selected = selected.unwrap();
        assert_eq!(selected.recipient_letter, 'D');
        assert_eq!(selected.recipient, data);
    }

    #[test]
    fn final_volume_uses_the_largest_current_provider_extent_within_the_authorized_end() {
        let extents = [
            FreeExtent {
                offset_bytes: 4096,
                length_bytes: 63 * 1024,
            },
            FreeExtent {
                offset_bytes: 2_000_123,
                length_bytes: 90_000_321,
            },
            FreeExtent {
                offset_bytes: 200_000_000,
                length_bytes: 900_000_000,
            },
        ];
        assert_eq!(
            select_current_free_extent(&extents, 100_000_444, 80_000_000, 0)
                .unwrap()
                .unwrap(),
            extents[1]
        );
        assert!(
            select_current_free_extent(&extents, 100_000_444, 91_000_000, 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn current_extent_is_intersected_and_reserves_following_boot_minimums() {
        let extent = FreeExtent {
            offset_bytes: 4096,
            length_bytes: 1_000_000,
        };
        assert_eq!(
            select_current_free_extent(&[extent], 900_123, 300_000, 400_000)
                .unwrap()
                .unwrap(),
            FreeExtent {
                offset_bytes: 4096,
                length_bytes: 496_027,
            }
        );
        assert!(
            select_current_free_extent(&[extent], 700_000, 300_000, 400_000)
                .unwrap()
                .is_none()
        );
        assert!(select_current_free_extent(&[extent], u64::MAX, u64::MAX, 1).is_err());
    }

    #[test]
    fn full_disk_functional_minimum_uses_sector_and_cross_version_requirements() {
        let image_minimum = 20 * 1024 * 1024 * 1024;
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::Windows, image_minimum, None)
                .unwrap(),
            image_minimum
        );
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::EfiSystem, image_minimum, Some(512))
                .unwrap(),
            ESP_512_MINIMUM_BYTES
        );
        assert_eq!(
            partition_functional_minimum(
                PlannedPartitionRole::EfiSystem,
                image_minimum,
                Some(4096)
            )
            .unwrap(),
            ESP_4KN_MINIMUM_BYTES
        );
        assert!(
            partition_functional_minimum(PlannedPartitionRole::EfiSystem, image_minimum, None)
                .is_err()
        );
        assert!(partition_functional_minimum(
            PlannedPartitionRole::EfiSystem,
            image_minimum,
            Some(2048)
        )
        .is_err());
        assert_eq!(
            partition_functional_minimum(
                PlannedPartitionRole::MicrosoftReserved,
                image_minimum,
                None
            )
            .unwrap(),
            MSR_WINDOWS_7_MINIMUM_BYTES
        );
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::SystemReserved, image_minimum, None)
                .unwrap(),
            BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES
        );
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::Data, image_minimum, None).unwrap(),
            MIN_USEFUL_DATA_BYTES
        );
    }

    #[test]
    fn optional_data_does_not_take_budget_from_windows() {
        let future = [PlannedPartition {
            offset_bytes: 123,
            length_bytes: MIN_USEFUL_DATA_BYTES,
            role: PlannedPartitionRole::Data,
        }];
        assert_eq!(
            following_required_minimum(&future, 20 * 1024 * 1024 * 1024, None, false).unwrap(),
            0
        );
        assert_eq!(
            following_required_minimum(&future, 20 * 1024 * 1024 * 1024, None, true).unwrap(),
            MIN_USEFUL_DATA_BYTES
        );
    }

    #[test]
    fn dual_boot_rollback_rejects_source_rebinding_after_tail_deletes() {
        let expected = VolumeIdentity {
            disk_number: 7,
            offset_bytes: 1_048_576 + 512,
            extent_length_bytes: 80_000_000_321,
        };
        assert!(ensure_rollback_source_unchanged(expected, expected).is_ok());
        assert!(ensure_rollback_source_unchanged(
            expected,
            VolumeIdentity {
                disk_number: 8,
                ..expected
            }
        )
        .is_err());
        assert!(ensure_rollback_source_unchanged(
            expected,
            VolumeIdentity {
                extent_length_bytes: expected.extent_length_bytes - 512,
                ..expected
            }
        )
        .is_err());
    }

    #[test]
    fn preserved_staging_offsets_are_compared_only_on_the_same_current_disk() {
        let staging = lr_core::custom_install::PreservedStagingExtent {
            disk_locator_token: "data".into(),
            offset_bytes: 10_000,
            length_bytes: 20_000,
        };
        let target = VolumeIdentity {
            disk_number: 7,
            offset_bytes: 15_000,
            extent_length_bytes: 2_000,
        };
        assert!(validate_preserved_staging_bounds(&staging, 100_000, 8, target).is_ok());
        assert!(validate_preserved_staging_bounds(&staging, 100_000, 7, target).is_err());
        assert!(validate_preserved_staging_bounds(&staging, 29_999, 8, target).is_err());
    }
}
