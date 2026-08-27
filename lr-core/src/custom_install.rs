//! Shared, disk-I/O-free contract for reinstall, full-disk reinstall and dual boot.
//!
//! Disk numbers in this protocol are diagnostics for the normal Windows session only.  A PE
//! worker must bind every destructive target through the authenticated random locator tokens and
//! then inspect the current extents once immediately before the first write.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::handoff_auth::validate_locator_token;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;
pub const IMAGE_ALLOWANCE_BYTES: u64 = 2 * GIB;
pub const OPAQUE_IMAGE_FALLBACK_BYTES: u64 = 80 * GIB;
pub const MIN_USEFUL_DATA_BYTES: u64 = 4 * GIB;
/// Current Microsoft deployment minimum for an ESP on 512-native/512e storage.
pub const ESP_512_MINIMUM_BYTES: u64 = 200 * MIB;
/// Current Microsoft deployment minimum for an ESP on 4K-native storage.
pub const ESP_4KN_MINIMUM_BYTES: u64 = 300 * MIB;
/// Windows 7 requires a 128-MiB MSR on GPT disks at least 16 GiB in size.  Keep the
/// cross-version Windows 7-11 layout at that larger value rather than using the newer 16-MiB
/// Windows 10/11-only recommendation.
pub const MSR_WINDOWS_7_MINIMUM_BYTES: u64 = 128 * MIB;
/// Microsoft documents 100 MiB as the BIOS boot-only minimum, while BitLocker requires a
/// separate NTFS system volume of about 350 MiB.  Full-disk installs support BitLocker, so the
/// smaller boot-only figure is not the functional minimum.
pub const BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES: u64 = 350 * MIB;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomInstallMode {
    #[default]
    ReinstallPartition,
    RepartitionAllDisks,
    DualBoot,
}

impl CustomInstallMode {
    pub const fn as_config_value(self) -> &'static str {
        match self {
            Self::ReinstallPartition => "reinstall_partition",
            Self::RepartitionAllDisks => "repartition_all_disks",
            Self::DualBoot => "dual_boot",
        }
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value.trim() {
            "reinstall_partition" => Some(Self::ReinstallPartition),
            "repartition_all_disks" => Some(Self::RepartitionAllDisks),
            "dual_boot" => Some(Self::DualBoot),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedPartitionStyle {
    Gpt,
    Mbr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullDiskRole {
    Windows,
    Data,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlannedPartitionRole {
    EfiSystem,
    MicrosoftReserved,
    SystemReserved,
    Windows,
    Data,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlannedPartition {
    pub offset_bytes: u64,
    pub length_bytes: u64,
    pub role: PlannedPartitionRole,
}

/// One disk explicitly shown in and accepted by the full-disk confirmation UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FullDiskSelection {
    /// Current-session diagnostic only. PE must not use it as cross-reboot authorization.
    pub diagnostic_disk_number: u32,
    /// Independent CNG-random locator written to one existing volume on this selected disk.
    pub locator_token: String,
    pub style: RequestedPartitionStyle,
    pub role: FullDiskRole,
}

/// Existing same-disk staging geometry. Existing extents are deliberately not required to be
/// MiB-aligned; only partitions newly created by this task carry that requirement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreservedStagingExtent {
    /// Locator of the selected disk that currently contains this existing staging extent.
    pub disk_locator_token: String,
    pub offset_bytes: u64,
    pub length_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepartitionAllDisksPlan {
    pub disks: Vec<FullDiskSelection>,
    /// Non-zero minimum capacity of the new Windows basic-data partition. Whether Windows uses
    /// all remaining space is decided by the concrete layout result, never encoded as zero.
    pub windows_partition_bytes: u64,
    /// Present only when the authenticated data locator is on the Windows target disk.
    pub preserved_staging: Option<PreservedStagingExtent>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DualBootPlan {
    /// Normal-Windows source volume locator used only before reboot and for rollback diagnostics.
    pub source_drive_letter: char,
    pub source_offset_bytes: u64,
    pub source_length_before_bytes: u64,
    /// Actual source length read back immediately after the normal-Windows shrink. Before the
    /// first preparation attempt this is the requested post-shrink length; the handoff must carry
    /// the provider's actual value.
    pub source_length_after_bytes: u64,
    /// Target and optional data volumes are created and read back before the PE handoff.
    pub target_offset_bytes: u64,
    pub target_length_bytes: u64,
    pub data_offset_bytes: Option<u64>,
    pub data_length_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "plan", rename_all = "snake_case")]
pub enum CustomInstallPlan {
    ReinstallPartition,
    RepartitionAllDisks(RepartitionAllDisksPlan),
    DualBoot(DualBootPlan),
}

impl Default for CustomInstallPlan {
    fn default() -> Self {
        Self::ReinstallPartition
    }
}

impl CustomInstallPlan {
    pub const fn mode(&self) -> CustomInstallMode {
        match self {
            Self::ReinstallPartition => CustomInstallMode::ReinstallPartition,
            Self::RepartitionAllDisks(_) => CustomInstallMode::RepartitionAllDisks,
            Self::DualBoot(_) => CustomInstallMode::DualBoot,
        }
    }

    pub fn validate(&self) -> Result<(), CustomInstallPlanError> {
        match self {
            Self::ReinstallPartition => Ok(()),
            Self::RepartitionAllDisks(plan) => validate_full_disk_plan(plan),
            Self::DualBoot(plan) => validate_dual_boot_plan(plan),
        }
    }

    pub fn to_json(&self) -> Result<String, CustomInstallPlanError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| CustomInstallPlanError::MalformedPlan)
    }

    pub fn from_json(value: &str) -> Result<Self, CustomInstallPlanError> {
        let plan: Self =
            serde_json::from_str(value).map_err(|_| CustomInstallPlanError::MalformedPlan)?;
        plan.validate()?;
        Ok(plan)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageSpaceSource {
    WimMetadata,
    OpaqueOrMissingMetadata,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageSpaceRequirement {
    pub windows_partition_bytes: u64,
    pub expanded_bytes: Option<u64>,
    pub source: ImageSpaceSource,
}

/// Compute `TOTALBYTES - HARDLINKBYTES + 2 GiB` without percentage or generation floors.
pub fn image_space_requirement(total_bytes: u64, hardlink_bytes: u64) -> ImageSpaceRequirement {
    let Some(expanded) = total_bytes.checked_sub(hardlink_bytes) else {
        return ImageSpaceRequirement::fallback();
    };
    if expanded == 0 {
        return ImageSpaceRequirement::fallback();
    }
    let required = expanded.saturating_add(IMAGE_ALLOWANCE_BYTES);
    ImageSpaceRequirement {
        // This is a capacity requirement, not a disk-geometry request. Keep the exact metadata
        // result; VDS/provider geometry is selected later from the current free extents.
        windows_partition_bytes: required,
        expanded_bytes: Some(expanded),
        source: ImageSpaceSource::WimMetadata,
    }
}

/// Plan only partitions created by the current full-disk transaction. `usable_end_bytes` may be
/// an existing staging offset that is not MiB-aligned. Fixed infrastructure sizes and the initial
/// placement preference remain conventional, but the exact Windows capacity requirement is not
/// rounded or rejected for cosmetic alignment; VDS returns the actual legal geometry.
pub fn plan_full_disk_layout(
    style: RequestedPartitionStyle,
    role: FullDiskRole,
    usable_end_bytes: u64,
    windows_partition_bytes: u64,
) -> Result<Vec<PlannedPartition>, CustomInstallPlanError> {
    // `usable_end_bytes` is either the provider's current disk capacity or the exact start of an
    // existing staging extent.  Flooring it to a whole MiB discards legal provider space and can
    // reject a disk that fits the exact image requirement by less than one cosmetic alignment
    // unit.  Keep the real boundary; only the initial placement remains a 1 MiB preference.
    let limit = usable_end_bytes;
    let mut cursor = MIB;
    let mut output = Vec::new();
    if role == FullDiskRole::Data {
        // Microsoft requires an MSR on every GPT disk. A data-only GPT disk has no ESP, so its
        // Windows 7-11-compatible MSR is the first partition.
        if style == RequestedPartitionStyle::Gpt {
            let end = cursor
                .checked_add(MSR_WINDOWS_7_MINIMUM_BYTES)
                .ok_or(CustomInstallPlanError::InvalidDiskCapacity)?;
            if end > limit {
                return Err(CustomInstallPlanError::InvalidDiskCapacity);
            }
            output.push(PlannedPartition {
                offset_bytes: cursor,
                length_bytes: MSR_WINDOWS_7_MINIMUM_BYTES,
                role: PlannedPartitionRole::MicrosoftReserved,
            });
            cursor = end;
        }
        let length = limit
            .checked_sub(cursor)
            .ok_or(CustomInstallPlanError::InvalidDiskCapacity)?;
        if length < MIN_USEFUL_DATA_BYTES {
            return Err(CustomInstallPlanError::InvalidDiskCapacity);
        }
        output.push(PlannedPartition {
            offset_bytes: cursor,
            length_bytes: length,
            role: PlannedPartitionRole::Data,
        });
        return Ok(output);
    }
    if windows_partition_bytes == 0 {
        return Err(CustomInstallPlanError::InvalidWindowsPartitionSize);
    }
    let infrastructure = match style {
        RequestedPartitionStyle::Gpt => [
            Some((ESP_4KN_MINIMUM_BYTES, PlannedPartitionRole::EfiSystem)),
            Some((
                MSR_WINDOWS_7_MINIMUM_BYTES,
                PlannedPartitionRole::MicrosoftReserved,
            )),
        ],
        RequestedPartitionStyle::Mbr => [
            Some((550 * MIB, PlannedPartitionRole::SystemReserved)),
            None,
        ],
    };
    for value in infrastructure.into_iter().flatten() {
        let end = cursor
            .checked_add(value.0)
            .ok_or(CustomInstallPlanError::InvalidDiskCapacity)?;
        if end > limit {
            return Err(CustomInstallPlanError::InvalidDiskCapacity);
        }
        output.push(PlannedPartition {
            offset_bytes: cursor,
            length_bytes: value.0,
            role: value.1,
        });
        cursor = end;
    }
    let available = limit
        .checked_sub(cursor)
        .ok_or(CustomInstallPlanError::InvalidDiskCapacity)?;
    // A non-zero value is the minimum capacity derived from the selected image metadata (or the
    // documented opaque-image fallback).  Giving Windows the whole remaining disk is only a
    // substitute for an optional data partition; it must never silently shrink this minimum.
    if windows_partition_bytes > available {
        return Err(CustomInstallPlanError::InvalidDiskCapacity);
    }
    let requested = if available.saturating_sub(windows_partition_bytes) < MIN_USEFUL_DATA_BYTES {
        available
    } else {
        windows_partition_bytes
    };
    if requested == 0 || requested > available {
        return Err(CustomInstallPlanError::InvalidDiskCapacity);
    }
    output.push(PlannedPartition {
        offset_bytes: cursor,
        length_bytes: requested,
        role: PlannedPartitionRole::Windows,
    });
    cursor = cursor
        .checked_add(requested)
        .ok_or(CustomInstallPlanError::InvalidDiskCapacity)?;
    let tail = limit.saturating_sub(cursor);
    if tail >= MIN_USEFUL_DATA_BYTES {
        output.push(PlannedPartition {
            offset_bytes: cursor,
            length_bytes: tail,
            role: PlannedPartitionRole::Data,
        });
    }
    Ok(output)
}

impl ImageSpaceRequirement {
    pub const fn fallback() -> Self {
        Self {
            windows_partition_bytes: OPAQUE_IMAGE_FALLBACK_BYTES,
            expanded_bytes: None,
            source: ImageSpaceSource::OpaqueOrMissingMetadata,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CustomInstallPlanError {
    EmptyDiskSet,
    DuplicateDiskLocator,
    DuplicateDiagnosticDisk,
    MissingWindowsDisk,
    MultipleWindowsDisks,
    InvalidWindowsPartitionSize,
    InvalidPreservedStaging,
    InvalidDualBootSource,
    InvalidDualBootTarget,
    DualBootExtentsOverlap,
    InvalidDiskCapacity,
    MalformedPlan,
}

impl std::fmt::Display for CustomInstallPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use CustomInstallPlanError::*;
        f.write_str(match self {
            EmptyDiskSet => "full-disk plan contains no explicitly confirmed disks",
            DuplicateDiskLocator => "full-disk plan contains a duplicate random disk locator",
            DuplicateDiagnosticDisk => "full-disk plan contains the same displayed disk twice",
            MissingWindowsDisk => "full-disk plan has no Windows target disk",
            MultipleWindowsDisks => "full-disk plan has more than one Windows target disk",
            InvalidWindowsPartitionSize => "Windows partition size is invalid",
            InvalidPreservedStaging => "preserved staging extent is empty or overflows",
            InvalidDualBootSource => "dual-boot source extent is invalid",
            InvalidDualBootTarget => "dual-boot target extent is invalid",
            DualBootExtentsOverlap => "dual-boot source, target or data extents overlap",
            InvalidDiskCapacity => "selected disk cannot contain the requested layout",
            MalformedPlan => "custom installation plan is malformed",
        })
    }
}

impl std::error::Error for CustomInstallPlanError {}

pub fn validate_full_disk_plan(
    plan: &RepartitionAllDisksPlan,
) -> Result<(), CustomInstallPlanError> {
    if plan.disks.is_empty() {
        return Err(CustomInstallPlanError::EmptyDiskSet);
    }
    let mut tokens = BTreeSet::new();
    let mut diagnostics = BTreeSet::new();
    let mut windows = 0usize;
    for disk in &plan.disks {
        validate_locator_token(&disk.locator_token)
            .map_err(|_| CustomInstallPlanError::DuplicateDiskLocator)?;
        if !tokens.insert(disk.locator_token.as_str()) {
            return Err(CustomInstallPlanError::DuplicateDiskLocator);
        }
        if !diagnostics.insert(disk.diagnostic_disk_number) {
            return Err(CustomInstallPlanError::DuplicateDiagnosticDisk);
        }
        if disk.role == FullDiskRole::Windows {
            windows += 1;
        }
    }
    match windows {
        0 => return Err(CustomInstallPlanError::MissingWindowsDisk),
        1 => {}
        _ => return Err(CustomInstallPlanError::MultipleWindowsDisks),
    }
    if plan.windows_partition_bytes == 0 {
        return Err(CustomInstallPlanError::InvalidWindowsPartitionSize);
    }
    if let Some(staging) = &plan.preserved_staging {
        if !tokens.contains(staging.disk_locator_token.as_str()) {
            return Err(CustomInstallPlanError::InvalidPreservedStaging);
        }
        if staging.offset_bytes == 0
            || staging.length_bytes == 0
            || staging
                .offset_bytes
                .checked_add(staging.length_bytes)
                .is_none()
        {
            return Err(CustomInstallPlanError::InvalidPreservedStaging);
        }
    }
    Ok(())
}

pub fn validate_dual_boot_plan(plan: &DualBootPlan) -> Result<(), CustomInstallPlanError> {
    if !plan.source_drive_letter.is_ascii_alphabetic()
        || plan.source_offset_bytes == 0
        || plan.source_length_before_bytes == 0
        || plan.source_length_after_bytes == 0
        || plan.source_length_after_bytes >= plan.source_length_before_bytes
        || plan
            .source_offset_bytes
            .checked_add(plan.source_length_before_bytes)
            .is_none()
    {
        return Err(CustomInstallPlanError::InvalidDualBootSource);
    }
    if plan.target_offset_bytes == 0 || plan.target_length_bytes == 0 {
        return Err(CustomInstallPlanError::InvalidDualBootTarget);
    }
    let source_end = plan
        .source_offset_bytes
        .checked_add(plan.source_length_after_bytes)
        .ok_or(CustomInstallPlanError::InvalidDualBootSource)?;
    if plan.target_offset_bytes < source_end {
        return Err(CustomInstallPlanError::DualBootExtentsOverlap);
    }
    if ranges_overlap(
        plan.source_offset_bytes,
        plan.source_length_after_bytes,
        plan.target_offset_bytes,
        plan.target_length_bytes,
    )? {
        return Err(CustomInstallPlanError::DualBootExtentsOverlap);
    }
    if let Some(data_offset) = plan.data_offset_bytes {
        if plan.data_length_bytes == 0
            || ranges_overlap(
                plan.target_offset_bytes,
                plan.target_length_bytes,
                data_offset,
                plan.data_length_bytes,
            )?
        {
            return Err(CustomInstallPlanError::DualBootExtentsOverlap);
        }
    } else if plan.data_length_bytes != 0 {
        return Err(CustomInstallPlanError::InvalidDualBootTarget);
    }
    Ok(())
}

/// Select the first provider-reported free extent at or after the current source end that can
/// contain the requested minimum. The provider offset is returned unchanged: a small legal gap or
/// a non-MiB boundary is not an error and must not be rounded into a different range.
pub fn select_dual_boot_free_extent(
    source_end_bytes: u64,
    required_bytes: u64,
    extents: &[crate::windows_storage::FreeExtent],
) -> Option<crate::windows_storage::FreeExtent> {
    if required_bytes == 0 {
        return None;
    }
    extents
        .iter()
        .copied()
        .filter(|extent| {
            extent.length_bytes >= required_bytes
                && extent.offset_bytes >= source_end_bytes
                && extent
                    .offset_bytes
                    .checked_add(extent.length_bytes)
                    .is_some()
        })
        .min_by_key(|extent| extent.offset_bytes)
}

/// Validate one currently existing preserved staging extent without imposing creation alignment.
pub fn validate_existing_staging_extent(
    staging: &PreservedStagingExtent,
    disk_size_bytes: u64,
    target_offset_bytes: u64,
    target_length_bytes: u64,
) -> Result<(), CustomInstallPlanError> {
    let staging_end = staging
        .offset_bytes
        .checked_add(staging.length_bytes)
        .ok_or(CustomInstallPlanError::InvalidPreservedStaging)?;
    if staging.offset_bytes == 0
        || staging.length_bytes == 0
        || staging_end > disk_size_bytes
        || ranges_overlap(
            staging.offset_bytes,
            staging.length_bytes,
            target_offset_bytes,
            target_length_bytes,
        )?
    {
        return Err(CustomInstallPlanError::InvalidPreservedStaging);
    }
    Ok(())
}

pub fn ranges_overlap(
    left_offset: u64,
    left_length: u64,
    right_offset: u64,
    right_length: u64,
) -> Result<bool, CustomInstallPlanError> {
    if left_length == 0 || right_length == 0 {
        return Err(CustomInstallPlanError::InvalidPreservedStaging);
    }
    let left_end = left_offset
        .checked_add(left_length)
        .ok_or(CustomInstallPlanError::InvalidPreservedStaging)?;
    let right_end = right_offset
        .checked_add(right_length)
        .ok_or(CustomInstallPlanError::InvalidPreservedStaging)?;
    Ok(left_offset < right_end && right_offset < left_end)
}

/// Return marker/config pairs whose non-empty random value is exactly equal. Different stale
/// values are normal environmental noise and are intentionally ignored.
pub fn exact_session_pair_indices(
    marker_sessions: &[&str],
    config_sessions: &[&str],
) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    for (marker_index, marker) in marker_sessions.iter().enumerate() {
        if marker.is_empty() {
            continue;
        }
        for (config_index, config) in config_sessions.iter().enumerate() {
            if !config.is_empty() && marker == config {
                matches.push((marker_index, config_index));
            }
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_A: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const TOKEN_B: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

    fn disk(number: u32, token: &str, role: FullDiskRole) -> FullDiskSelection {
        FullDiskSelection {
            diagnostic_disk_number: number,
            locator_token: token.to_owned(),
            style: RequestedPartitionStyle::Gpt,
            role,
        }
    }

    #[test]
    fn full_disk_plan_has_exactly_one_windows_target() {
        let plan = RepartitionAllDisksPlan {
            disks: vec![
                disk(0, TOKEN_A, FullDiskRole::Windows),
                disk(1, TOKEN_B, FullDiskRole::Data),
            ],
            windows_partition_bytes: 64 * GIB,
            preserved_staging: None,
        };
        assert_eq!(validate_full_disk_plan(&plan), Ok(()));
        let mut invalid = plan.clone();
        invalid.disks[1].role = FullDiskRole::Windows;
        assert_eq!(
            validate_full_disk_plan(&invalid),
            Err(CustomInstallPlanError::MultipleWindowsDisks)
        );

        let mut zero_minimum = plan;
        zero_minimum.windows_partition_bytes = 0;
        assert_eq!(
            validate_full_disk_plan(&zero_minimum),
            Err(CustomInstallPlanError::InvalidWindowsPartitionSize)
        );
        let unvalidated_json = serde_json::to_string(&CustomInstallPlan::RepartitionAllDisks(
            zero_minimum.clone(),
        ))
        .unwrap();
        assert_eq!(
            CustomInstallPlan::from_json(&unvalidated_json),
            Err(CustomInstallPlanError::InvalidWindowsPartitionSize)
        );
        assert_eq!(
            CustomInstallPlan::RepartitionAllDisks(zero_minimum)
                .to_json()
                .unwrap_err(),
            CustomInstallPlanError::InvalidWindowsPartitionSize
        );
    }

    #[test]
    fn image_budget_is_total_minus_hardlinks_plus_two_gib() {
        let requirement = image_space_requirement(30 * GIB, 9 * GIB);
        assert_eq!(requirement.expanded_bytes, Some(21 * GIB));
        assert_eq!(requirement.windows_partition_bytes, 23 * GIB);
        assert_eq!(
            image_space_requirement(0, 0),
            ImageSpaceRequirement::fallback()
        );
        assert_eq!(
            image_space_requirement(10, 11),
            ImageSpaceRequirement::fallback()
        );
        let non_mib = image_space_requirement(30 * GIB + 4096, 9 * GIB);
        assert_eq!(non_mib.windows_partition_bytes, 23 * GIB + 4096);
    }

    #[test]
    fn exact_eighteen_gib_expansion_stays_twenty_gib_through_handoff_json() {
        let requirement = image_space_requirement(18 * GIB, 0);
        assert_eq!(requirement.expanded_bytes, Some(18 * GIB));
        assert_eq!(requirement.windows_partition_bytes, 20 * GIB);

        let plan = CustomInstallPlan::DualBoot(DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: MIB + 512,
            source_length_before_bytes: 200 * GIB,
            source_length_after_bytes: 180 * GIB,
            target_offset_bytes: MIB + 512 + 180 * GIB,
            target_length_bytes: requirement.windows_partition_bytes,
            data_offset_bytes: None,
            data_length_bytes: 0,
        });
        let json = plan.to_json().unwrap();
        assert_eq!(CustomInstallPlan::from_json(&json).unwrap(), plan);
    }

    #[test]
    fn full_disk_accepts_exact_existing_staging_extent_without_mib_alignment() {
        let staging = PreservedStagingExtent {
            disk_locator_token: TOKEN_A.to_owned(),
            offset_bytes: 700 * GIB + 512 * 1024,
            length_bytes: 100 * GIB + 4096,
        };
        assert_eq!(
            validate_existing_staging_extent(&staging, 1024 * GIB, MIB, 100 * GIB),
            Ok(())
        );
    }

    #[test]
    fn existing_staging_rejects_missing_overflow_out_of_disk_and_overlap() {
        assert!(validate_existing_staging_extent(
            &PreservedStagingExtent {
                disk_locator_token: TOKEN_A.to_owned(),
                offset_bytes: 0,
                length_bytes: GIB,
            },
            100 * GIB,
            MIB,
            20 * GIB,
        )
        .is_err());
        assert!(validate_existing_staging_extent(
            &PreservedStagingExtent {
                disk_locator_token: TOKEN_A.to_owned(),
                offset_bytes: u64::MAX - 1,
                length_bytes: 2,
            },
            u64::MAX,
            MIB,
            20 * GIB,
        )
        .is_err());
        assert!(validate_existing_staging_extent(
            &PreservedStagingExtent {
                disk_locator_token: TOKEN_A.to_owned(),
                offset_bytes: 90 * GIB,
                length_bytes: 20 * GIB,
            },
            100 * GIB,
            MIB,
            20 * GIB,
        )
        .is_err());
        assert!(validate_existing_staging_extent(
            &PreservedStagingExtent {
                disk_locator_token: TOKEN_A.to_owned(),
                offset_bytes: 10 * GIB,
                length_bytes: 20 * GIB,
            },
            100 * GIB,
            MIB,
            20 * GIB,
        )
        .is_err());
    }

    #[test]
    fn requested_windows_size_is_not_an_alignment_gate() {
        let plan = RepartitionAllDisksPlan {
            disks: vec![disk(0, TOKEN_A, FullDiskRole::Windows)],
            windows_partition_bytes: 64 * GIB + 1,
            preserved_staging: None,
        };
        assert_eq!(validate_full_disk_plan(&plan), Ok(()));
    }

    #[test]
    fn dual_boot_uses_provider_extent_without_rounding_a_non_mib_offset() {
        let source_end = 100 * GIB + 512 * 1024;
        let provider = crate::windows_storage::FreeExtent {
            offset_bytes: source_end + 4096,
            length_bytes: 40 * GIB + 12345,
        };
        assert_eq!(
            select_dual_boot_free_extent(source_end, 30 * GIB, &[provider]),
            Some(provider)
        );
    }

    #[test]
    fn dual_boot_accepts_logged_provider_geometry_that_differs_from_the_desired_request() {
        // Real provider result: VDS moved the desired offset forward by 16,896 bytes and made
        // the partition 1 MiB smaller.  Desired geometry is not an authorization boundary; the
        // provider extent and the image-derived functional capacity are.
        let desired_offset = 99_697_540_608;
        let desired_size = 7_676_624_896;
        let source_offset = MIB;
        let source_length_after = desired_offset - source_offset;
        let provider = crate::windows_storage::FreeExtent {
            offset_bytes: 99_697_557_504,
            length_bytes: 7_675_576_320,
        };
        assert_eq!(provider.offset_bytes - desired_offset, 16_896);
        assert_eq!(desired_size - provider.length_bytes, MIB);
        assert_eq!(
            select_dual_boot_free_extent(desired_offset, provider.length_bytes, &[provider]),
            Some(provider)
        );

        let observed_plan = DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: source_offset,
            source_length_before_bytes: source_length_after + 2 * GIB,
            source_length_after_bytes: source_length_after,
            target_offset_bytes: provider.offset_bytes,
            target_length_bytes: provider.length_bytes,
            data_offset_bytes: None,
            data_length_bytes: 0,
        };
        assert_eq!(validate_dual_boot_plan(&observed_plan), Ok(()));

        let mut overlapping = observed_plan.clone();
        overlapping.target_offset_bytes = desired_offset - 1;
        assert_eq!(
            validate_dual_boot_plan(&overlapping),
            Err(CustomInstallPlanError::DualBootExtentsOverlap)
        );
        let overflowing_provider = crate::windows_storage::FreeExtent {
            offset_bytes: u64::MAX - 7,
            length_bytes: 8,
        };
        assert_eq!(
            select_dual_boot_free_extent(1, 1, &[overflowing_provider]),
            None
        );
    }

    #[test]
    fn full_disk_gpt_layout_keeps_windows_7_compatible_msr_capacity() {
        let layout = plan_full_disk_layout(
            RequestedPartitionStyle::Gpt,
            FullDiskRole::Windows,
            128 * GIB,
            64 * GIB,
        )
        .unwrap();
        let msr = layout
            .iter()
            .find(|partition| partition.role == PlannedPartitionRole::MicrosoftReserved)
            .unwrap();
        assert_eq!(msr.length_bytes, MSR_WINDOWS_7_MINIMUM_BYTES);
    }

    #[test]
    fn data_only_gpt_disk_has_an_msr_but_no_esp() {
        let layout = plan_full_disk_layout(
            RequestedPartitionStyle::Gpt,
            FullDiskRole::Data,
            64 * GIB,
            32 * GIB,
        )
        .unwrap();
        assert_eq!(
            layout
                .iter()
                .map(|partition| partition.role)
                .collect::<Vec<_>>(),
            vec![
                PlannedPartitionRole::MicrosoftReserved,
                PlannedPartitionRole::Data,
            ]
        );
        assert_eq!(layout[0].length_bytes, MSR_WINDOWS_7_MINIMUM_BYTES);
        assert_eq!(layout[1].offset_bytes, MIB + MSR_WINDOWS_7_MINIMUM_BYTES);
        assert_eq!(layout[1].offset_bytes + layout[1].length_bytes, 64 * GIB);
    }

    #[test]
    fn full_disk_layout_keeps_exact_capacity_before_unaligned_staging() {
        let staging_offset = 700 * GIB + 512 * 1024;
        let requested = 64 * GIB + 4096;
        let layout = plan_full_disk_layout(
            RequestedPartitionStyle::Gpt,
            FullDiskRole::Windows,
            staging_offset,
            requested,
        )
        .unwrap();
        assert!(layout.iter().all(|partition| {
            partition.offset_bytes + partition.length_bytes <= staging_offset
        }));
        assert_eq!(
            layout
                .iter()
                .find(|partition| partition.role == PlannedPartitionRole::Windows)
                .unwrap()
                .length_bytes,
            requested
        );
        assert_eq!(
            layout
                .iter()
                .filter(|partition| partition.role == PlannedPartitionRole::Windows)
                .count(),
            1
        );
    }

    #[test]
    fn full_disk_layout_never_uses_zero_as_all_remaining_sentinel() {
        assert_eq!(
            plan_full_disk_layout(
                RequestedPartitionStyle::Gpt,
                FullDiskRole::Windows,
                256 * GIB,
                0,
            ),
            Err(CustomInstallPlanError::InvalidWindowsPartitionSize)
        );
    }

    #[test]
    fn full_disk_capacity_does_not_discard_a_legal_sub_mib_provider_tail() {
        let requested = 64 * GIB + 4096;
        let fixed_prefix = MIB + ESP_4KN_MINIMUM_BYTES + MSR_WINDOWS_7_MINIMUM_BYTES;
        let provider_end = fixed_prefix + requested + 512;
        let layout = plan_full_disk_layout(
            RequestedPartitionStyle::Gpt,
            FullDiskRole::Windows,
            provider_end,
            requested,
        )
        .expect("the exact provider capacity satisfies the image minimum");
        let windows = layout
            .iter()
            .find(|partition| partition.role == PlannedPartitionRole::Windows)
            .unwrap();
        assert_eq!(windows.offset_bytes, fixed_prefix);
        assert_eq!(windows.length_bytes, requested + 512);
        assert_eq!(windows.offset_bytes + windows.length_bytes, provider_end);
    }

    #[test]
    fn small_remainder_is_given_to_windows_instead_of_blocking_for_data() {
        let layout = plan_full_disk_layout(
            RequestedPartitionStyle::Gpt,
            FullDiskRole::Windows,
            70 * GIB,
            68 * GIB,
        )
        .unwrap();
        assert!(!layout
            .iter()
            .any(|partition| partition.role == PlannedPartitionRole::Data));
    }

    #[test]
    fn full_disk_rejects_capacity_below_the_image_minimum_instead_of_shrinking_it() {
        assert_eq!(
            plan_full_disk_layout(
                RequestedPartitionStyle::Gpt,
                FullDiskRole::Windows,
                64 * GIB,
                64 * GIB,
            ),
            Err(CustomInstallPlanError::InvalidDiskCapacity)
        );
    }

    #[test]
    fn install_mode_layout_matrix_accepts_common_and_rare_legal_geometries() {
        assert_eq!(CustomInstallPlan::ReinstallPartition.validate(), Ok(()));

        struct Case {
            style: RequestedPartitionStyle,
            role: FullDiskRole,
            usable_end: u64,
            windows_minimum: u64,
            expected_roles: &'static [PlannedPartitionRole],
        }
        let gpt_prefix = MIB + ESP_4KN_MINIMUM_BYTES + MSR_WINDOWS_7_MINIMUM_BYTES;
        let cases = [
            Case {
                style: RequestedPartitionStyle::Gpt,
                role: FullDiskRole::Windows,
                usable_end: 80 * GIB,
                windows_minimum: 24 * GIB,
                expected_roles: &[
                    PlannedPartitionRole::EfiSystem,
                    PlannedPartitionRole::MicrosoftReserved,
                    PlannedPartitionRole::Windows,
                    PlannedPartitionRole::Data,
                ],
            },
            // A sector-legal provider end need not be a whole MiB. The extra 512-byte tail is
            // useful Windows capacity, not an alignment error or a reason to invent a data volume.
            Case {
                style: RequestedPartitionStyle::Gpt,
                role: FullDiskRole::Windows,
                usable_end: gpt_prefix + 24 * GIB + 512,
                windows_minimum: 24 * GIB,
                expected_roles: &[
                    PlannedPartitionRole::EfiSystem,
                    PlannedPartitionRole::MicrosoftReserved,
                    PlannedPartitionRole::Windows,
                ],
            },
            Case {
                style: RequestedPartitionStyle::Mbr,
                role: FullDiskRole::Windows,
                usable_end: 80 * GIB + 4096,
                windows_minimum: 24 * GIB + 1,
                expected_roles: &[
                    PlannedPartitionRole::SystemReserved,
                    PlannedPartitionRole::Windows,
                    PlannedPartitionRole::Data,
                ],
            },
            Case {
                style: RequestedPartitionStyle::Gpt,
                role: FullDiskRole::Data,
                usable_end: MIB + MSR_WINDOWS_7_MINIMUM_BYTES + MIN_USEFUL_DATA_BYTES + 512,
                windows_minimum: 24 * GIB,
                expected_roles: &[
                    PlannedPartitionRole::MicrosoftReserved,
                    PlannedPartitionRole::Data,
                ],
            },
            Case {
                style: RequestedPartitionStyle::Mbr,
                role: FullDiskRole::Data,
                usable_end: MIB + MIN_USEFUL_DATA_BYTES + 4096,
                windows_minimum: 24 * GIB,
                expected_roles: &[PlannedPartitionRole::Data],
            },
        ];
        for case in cases {
            let layout =
                plan_full_disk_layout(case.style, case.role, case.usable_end, case.windows_minimum)
                    .unwrap();
            assert_eq!(
                layout
                    .iter()
                    .map(|partition| partition.role)
                    .collect::<Vec<_>>(),
                case.expected_roles
            );
            assert!(layout.windows(2).all(|pair| {
                pair[0].offset_bytes + pair[0].length_bytes == pair[1].offset_bytes
            }));
            assert!(layout.iter().all(|partition| {
                partition
                    .offset_bytes
                    .checked_add(partition.length_bytes)
                    .is_some_and(|end| end <= case.usable_end)
            }));
        }

        assert_eq!(
            plan_full_disk_layout(
                RequestedPartitionStyle::Gpt,
                FullDiskRole::Windows,
                gpt_prefix + 24 * GIB - 1,
                24 * GIB,
            ),
            Err(CustomInstallPlanError::InvalidDiskCapacity)
        );
        assert_eq!(
            plan_full_disk_layout(
                RequestedPartitionStyle::Gpt,
                FullDiskRole::Data,
                MIB + MSR_WINDOWS_7_MINIMUM_BYTES + MIN_USEFUL_DATA_BYTES - 1,
                24 * GIB,
            ),
            Err(CustomInstallPlanError::InvalidDiskCapacity)
        );
    }

    #[test]
    fn dual_boot_provider_extent_matrix_preserves_rare_boundaries_and_stops_unusable_ranges() {
        use crate::windows_storage::FreeExtent;

        let source_end = 40 * GIB + 512;
        let required = 24 * GIB + 1;
        let later = FreeExtent {
            offset_bytes: source_end + 16_896,
            length_bytes: required + 4096,
        };
        let exact = FreeExtent {
            offset_bytes: source_end,
            length_bytes: required,
        };
        let before_source = FreeExtent {
            offset_bytes: source_end - MIB,
            length_bytes: required + 2 * MIB,
        };
        let too_small = FreeExtent {
            offset_bytes: source_end + 512,
            length_bytes: required - 1,
        };

        assert_eq!(
            select_dual_boot_free_extent(
                source_end,
                required,
                &[later, too_small, before_source, exact],
            ),
            Some(exact)
        );
        assert_eq!(
            select_dual_boot_free_extent(source_end, required, &[later]),
            Some(later)
        );
        assert_eq!(
            select_dual_boot_free_extent(source_end, required, &[too_small, before_source]),
            None
        );
        assert_eq!(
            select_dual_boot_free_extent(
                source_end,
                required,
                &[FreeExtent {
                    offset_bytes: u64::MAX - 7,
                    length_bytes: required,
                }],
            ),
            None
        );
        assert_eq!(select_dual_boot_free_extent(source_end, 0, &[exact]), None);
    }

    #[test]
    fn session_pairing_ignores_old_different_values_and_rejects_ambiguity() {
        assert_eq!(
            exact_session_pair_indices(&["old", "wanted", "other"], &["wanted"]),
            vec![(1, 0)]
        );
        assert!(exact_session_pair_indices(&["old"], &["wanted"]).is_empty());
        assert_eq!(
            exact_session_pair_indices(&["wanted", "wanted"], &["wanted"]),
            vec![(0, 0), (1, 0)]
        );
    }

    #[test]
    fn plan_json_is_strongly_typed_and_round_trips() {
        let plan = CustomInstallPlan::RepartitionAllDisks(RepartitionAllDisksPlan {
            disks: vec![disk(0, TOKEN_A, FullDiskRole::Windows)],
            windows_partition_bytes: 80 * GIB,
            preserved_staging: None,
        });
        let json = plan.to_json().unwrap();
        assert!(!json.contains(['\r', '\n']));
        assert_eq!(CustomInstallPlan::from_json(&json).unwrap(), plan);
    }
}
