use crate::core::bitlocker::{BitLockerManager, VolumeStatus};
use crate::tr;
use anyhow::{Context, Result};
use lr_core::data_staging::{
    required_staging_bytes, select_staging_plan, ShrinkCandidate, StagingCandidate, StagingPlan,
    StorageAttachment, StorageMedia,
};
use std::path::Path;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Win32::Storage::FileSystem::{
        BusType1394, BusTypeFileBackedVirtual, BusTypeMmc, BusTypeNvme, BusTypeSCM, BusTypeSd,
        BusTypeUsb, BusTypeVirtual, CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW,
        GetVolumeInformationW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, StorageDeviceSeekPenaltyProperty,
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_GET_DEVICE_NUMBER,
        IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
        STORAGE_PROPERTY_QUERY,
    },
    Win32::System::IO::DeviceIoControl,
};

// 驱动器类型常量
#[allow(dead_code)]
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
#[allow(dead_code)]
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
#[allow(dead_code)]
const DRIVE_RAMDISK: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StagingDriveKind {
    Fixed,
    Removable,
}

fn classify_staging_drive_type(drive_type: u32) -> Option<StagingDriveKind> {
    match drive_type {
        DRIVE_FIXED => Some(StagingDriveKind::Fixed),
        DRIVE_REMOVABLE => Some(StagingDriveKind::Removable),
        _ => None,
    }
}

/// Windows supports shrinking an online NTFS BitLocker volume without decrypting it first.  Keep
/// that path limited to the current Windows volume while its conversion state is stable and the
/// volume is unlocked.  Locked, converting and unknown states remain fail-closed.
fn auto_shrink_target_is_safe(
    file_system: Option<&str>,
    bitlocker_status: VolumeStatus,
    attachment: StorageAttachment,
    is_current_system: bool,
) -> bool {
    let bitlocker_is_safe = bitlocker_status == VolumeStatus::NotEncrypted
        || (is_current_system && bitlocker_status == VolumeStatus::EncryptedUnlocked);
    bitlocker_is_safe
        && file_system.is_some_and(|name| name.eq_ignore_ascii_case("NTFS"))
        && attachment != StorageAttachment::External
}

/// 自动创建分区的标志文件名
pub const AUTO_CREATED_PARTITION_MARKER: &str = "LetRecovery_AutoCreated.marker";

/// 分区表类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// Keep the established names because they are shown verbatim throughout both endpoints.
#[allow(clippy::upper_case_acronyms)]
pub enum PartitionStyle {
    GPT,
    MBR,
    #[default]
    Unknown,
}

impl std::fmt::Display for PartitionStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionStyle::GPT => write!(f, "GPT"),
            PartitionStyle::MBR => write!(f, "MBR"),
            PartitionStyle::Unknown => write!(f, "{}", tr!("未知")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub letter: String,
    pub total_size_mb: u64,
    pub free_size_mb: u64,
    /// Exact current-session free bytes from GetDiskFreeSpaceExW. MiB is display-only.
    pub free_size_bytes: u64,
    pub label: String,
    pub is_system_partition: bool,
    pub has_windows: bool,
    pub partition_style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
    /// Stable geometry captured from the physical-disk IOCTL inventory.
    pub disk_size_bytes: Option<u64>,
    pub partition_offset_bytes: Option<u64>,
    pub partition_size_bytes: Option<u64>,
    /// Partition role captured from the same physical-disk layout as the exact geometry.
    /// `None` means that the role could not be established and must not be guessed.
    pub partition_kind: Option<lr_core::windows_storage::PartitionKind>,
    /// True only when the canonical current disk layout identifies this extent as ordinary
    /// installable user data. WinPE drive-letter assignment alone is never sufficient.
    pub install_target_eligible: bool,
    /// Read-only current-session media classification. Query failures remain `Unknown`.
    pub storage_media: StorageMedia,
    /// Immutable disk/partition token captured with this inventory snapshot.
    pub stable_identity: Option<lr_core::windows_storage::StableVolumeIdentity>,
    pub bitlocker_status: VolumeStatus,
}

/// 分区详细信息
#[derive(Debug, Clone)]
pub struct PartitionDetail {
    pub style: PartitionStyle,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
}

#[cfg(windows)]
fn validated_seek_penalty_descriptor(
    version: u32,
    size: u32,
    bytes_returned: u32,
    incurs_seek_penalty: bool,
) -> Option<bool> {
    let required = u32::try_from(std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()).ok()?;
    (version >= required && size >= required && bytes_returned >= required)
        .then_some(incurs_seek_penalty)
}

#[cfg(windows)]
fn validated_storage_device_number(
    bytes_returned: u32,
    device_number: u32,
    partition_number: u32,
) -> Option<(u32, u32)> {
    let required = u32::try_from(std::mem::size_of::<STORAGE_DEVICE_NUMBER>()).ok()?;
    (bytes_returned >= required && device_number != u32::MAX && partition_number != u32::MAX)
        .then_some((device_number, partition_number))
}

#[cfg(windows)]
fn validated_storage_bus_type(
    version: u32,
    size: u32,
    bytes_returned: u32,
    bus_type: i32,
) -> Option<i32> {
    let required = u32::try_from(std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>()).ok()?;
    (version >= required && size >= required && bytes_returned >= required).then_some(bus_type)
}

fn normalized_partition_letter(value: &str) -> Option<char> {
    let mut characters = value.chars();
    let letter = characters.next()?.to_ascii_uppercase();
    (letter.is_ascii_alphabetic() && characters.next() == Some(':') && characters.next().is_none())
        .then_some(letter)
}

/// Choose only a display default. The actual install path still performs its fresh stable-identity
/// and extent checks before any destructive work. In WinPE, an unresolved role is deliberately not
/// auto-selected because a drive letter can temporarily be assigned to an ESP by maintenance tools.
pub fn preferred_install_partition_index(
    partitions: &[Partition],
    is_pe_environment: bool,
) -> Option<usize> {
    if !is_pe_environment {
        return partitions.iter().position(|partition| {
            partition.is_system_partition && partition.install_target_eligible
        });
    }

    partitions
        .iter()
        .enumerate()
        .filter_map(|(index, partition)| {
            let letter = normalized_partition_letter(&partition.letter)?;
            if !partition.install_target_eligible {
                return None;
            }
            if letter == 'X' {
                return None;
            }
            let disk_number = partition.disk_number?;
            let partition_number = partition.partition_number?;
            let offset_bytes = partition.partition_offset_bytes?;
            let kind = partition.partition_kind?;
            if disk_number == u32::MAX
                || partition_number == u32::MAX
                || matches!(
                    kind,
                    lr_core::windows_storage::PartitionKind::EfiSystem
                        | lr_core::windows_storage::PartitionKind::MicrosoftReserved
                )
            {
                return None;
            }
            let non_ssd_rank = u8::from(partition.storage_media != StorageMedia::SolidState);
            Some((
                (
                    non_ssd_rank,
                    disk_number,
                    offset_bytes,
                    partition_number,
                    letter,
                ),
                index,
            ))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, index)| index)
}

#[must_use = "dual-boot preparation must be committed or rolled back"]
pub struct PreparedDualBootTransaction {
    plan: lr_core::custom_install::DualBootPlan,
    source_disk_number: u32,
    target_letter: char,
    data_letter: Option<char>,
    created_by_attempt: bool,
    active: bool,
}

#[must_use = "a staging shrink must be committed or rolled back"]
pub(super) struct PreparedStagingTransaction {
    source_letter: char,
    source_before: lr_core::windows_storage::VolumeIdentity,
    source_after: lr_core::windows_storage::VolumeIdentity,
    created: Option<lr_core::windows_storage::CreatedPartition>,
    target_letter: char,
    active: bool,
}

impl PreparedStagingTransaction {
    pub(super) fn target_partition(&self) -> String {
        format!("{}:", self.target_letter)
    }

    pub(super) fn source_length_before_bytes(&self) -> u64 {
        self.source_before.extent_length_bytes
    }

    pub(super) fn commit(mut self) {
        self.active = false;
    }

    #[cfg(feature = "ci-automation")]
    pub(super) fn ci_rollback_receipt(&self) -> String {
        let created = self
            .created
            .expect("an armed staging transaction must retain its created extent");
        format!(
            "source_disk={} source_offset={} source_before={} source_after={} created_offset={} created_size={} target_letter={}",
            self.source_before.disk_number,
            self.source_before.offset_bytes,
            self.source_before.extent_length_bytes,
            self.source_after.extent_length_bytes,
            created.offset_bytes,
            created.size_bytes,
            self.target_letter
        )
    }

    fn rollback_inner(&self) -> Result<()> {
        if let Some(created) = self.created {
            let records = lr_core::windows_storage::partitions(self.source_before.disk_number)?;
            if records.iter().any(|record| {
                record.offset_bytes == created.offset_bytes
                    && record.size_bytes == created.size_bytes
                    && record.kind == lr_core::windows_storage::PartitionKind::BasicData
            }) {
                let snapshot =
                    lr_core::windows_storage::disk_layout_snapshot(self.source_before.disk_number)?;
                lr_core::windows_storage::delete_partition_checked(
                    self.source_before.disk_number,
                    created.offset_bytes,
                    false,
                    &snapshot,
                )?;
            }
        }
        let source = lr_core::windows_storage::volume_identity(self.source_letter)?;
        if source != self.source_after {
            anyhow::bail!("staging rollback source extent changed");
        }
        let reclaimed = self
            .source_before
            .extent_length_bytes
            .checked_sub(self.source_after.extent_length_bytes)
            .context("staging rollback length underflow")?;
        lr_core::windows_storage::extend_volume_checked(self.source_letter, source, reclaimed)?;
        let restored = lr_core::windows_storage::volume_identity(self.source_letter)?;
        if restored != self.source_before {
            anyhow::bail!("staging rollback readback differs from the original source extent");
        }
        Ok(())
    }
}

impl Drop for PreparedStagingTransaction {
    fn drop(&mut self) {
        if self.active {
            if let Err(error) = self.rollback_inner() {
                log::error!("failed to roll back uncommitted staging partition: {error:#}");
            }
            self.active = false;
        }
    }
}

impl PreparedDualBootTransaction {
    pub fn target_partition(&self) -> String {
        format!("{}:", self.target_letter)
    }

    /// Return the optional data/staging volume created by the same shrink transaction.
    /// Keeping this letter inside the move-only transaction prevents the caller from discovering
    /// a different volume by path after the provider has returned the actual layout.
    pub fn data_partition(&self) -> Option<String> {
        self.data_letter.map(|letter| format!("{letter}:"))
    }

    pub fn plan(&self) -> &lr_core::custom_install::DualBootPlan {
        &self.plan
    }

    pub fn commit(mut self) {
        self.active = false;
    }

    pub fn rollback(mut self) -> Result<()> {
        let result = self.rollback_inner();
        self.active = false;
        result
    }

    fn rollback_inner(&self) -> Result<()> {
        if !self.created_by_attempt {
            return Ok(());
        }
        let source_letter = self.plan.source_drive_letter.to_ascii_uppercase();
        let source = lr_core::windows_storage::volume_identity(source_letter)?;
        if source.disk_number != self.source_disk_number
            || source.offset_bytes != self.plan.source_offset_bytes
            || source.extent_length_bytes != self.plan.source_length_after_bytes
        {
            anyhow::bail!("dual-boot rollback source extent changed");
        }
        let reclaimed = self
            .plan
            .source_length_before_bytes
            .checked_sub(self.plan.source_length_after_bytes)
            .context("dual-boot rollback length overflow")?;
        let tail_offset = self
            .plan
            .source_offset_bytes
            .checked_add(self.plan.source_length_after_bytes)
            .context("dual-boot rollback tail offset overflow")?;
        let records = lr_core::windows_storage::partitions(self.source_disk_number)?;
        for record in &records {
            let overlap = lr_core::custom_install::ranges_overlap(
                record.offset_bytes,
                record.size_bytes,
                tail_offset,
                reclaimed,
            )
            .map_err(anyhow::Error::msg)?;
            let owned_target = record.offset_bytes == self.plan.target_offset_bytes
                && record.size_bytes == self.plan.target_length_bytes
                && record.kind == lr_core::windows_storage::PartitionKind::BasicData;
            let owned_data = self.plan.data_offset_bytes.is_some_and(|offset| {
                record.offset_bytes == offset
                    && record.size_bytes == self.plan.data_length_bytes
                    && record.kind == lr_core::windows_storage::PartitionKind::BasicData
            });
            if overlap && !owned_target && !owned_data {
                anyhow::bail!("dual-boot rollback tail contains an unowned partition");
            }
        }
        if let Some(offset) = self.plan.data_offset_bytes {
            if records.iter().any(|record| {
                record.offset_bytes == offset
                    && record.size_bytes == self.plan.data_length_bytes
                    && record.kind == lr_core::windows_storage::PartitionKind::BasicData
            }) {
                let snapshot =
                    lr_core::windows_storage::disk_layout_snapshot(self.source_disk_number)?;
                lr_core::windows_storage::delete_partition_checked(
                    self.source_disk_number,
                    offset,
                    false,
                    &snapshot,
                )?;
            }
        }
        let records = lr_core::windows_storage::partitions(self.source_disk_number)?;
        if records.iter().any(|record| {
            record.offset_bytes == self.plan.target_offset_bytes
                && record.size_bytes == self.plan.target_length_bytes
                && record.kind == lr_core::windows_storage::PartitionKind::BasicData
        }) {
            let snapshot = lr_core::windows_storage::disk_layout_snapshot(self.source_disk_number)?;
            lr_core::windows_storage::delete_partition_checked(
                self.source_disk_number,
                self.plan.target_offset_bytes,
                false,
                &snapshot,
            )?;
        }
        let source = lr_core::windows_storage::volume_identity(source_letter)?;
        if source.disk_number != self.source_disk_number
            || source.offset_bytes != self.plan.source_offset_bytes
            || source.extent_length_bytes != self.plan.source_length_after_bytes
        {
            anyhow::bail!(
                "dual-boot rollback source rebound after partition cleanup; no extension attempted"
            );
        }
        lr_core::windows_storage::extend_volume_checked(source_letter, source, reclaimed)?;
        let restored = lr_core::windows_storage::volume_identity(source_letter)?;
        if restored.offset_bytes != self.plan.source_offset_bytes
            || restored.extent_length_bytes != self.plan.source_length_before_bytes
        {
            anyhow::bail!("dual-boot source rollback readback differs from its original extent");
        }
        Ok(())
    }
}

impl Drop for PreparedDualBootTransaction {
    fn drop(&mut self) {
        if self.active {
            if let Err(error) = self.rollback_inner() {
                log::error!("failed to roll back uncommitted dual-boot target: {error:#}");
            }
            self.active = false;
        }
    }
}

fn first_free_drive_letter() -> Result<char> {
    first_free_drive_letter_from_mask(
        lr_core::windows_storage::assigned_drive_letter_mask()?,
        None,
    )
}

fn first_free_drive_letter_excluding(excluded: char) -> Result<char> {
    first_free_drive_letter_from_mask(
        lr_core::windows_storage::assigned_drive_letter_mask()?,
        Some(excluded),
    )
}

fn first_free_drive_letter_from_mask(mask: u32, excluded: Option<char>) -> Result<char> {
    (b'D'..=b'Z')
        .rev()
        .map(char::from)
        .find(|letter| {
            Some(*letter) != excluded && mask & (1_u32 << u32::from(*letter as u8 - b'A')) == 0
        })
        .context("no unused drive letter is available for the dual-boot target")
}

fn observed_shrink_bytes(
    before: lr_core::windows_storage::VolumeIdentity,
    current: lr_core::windows_storage::VolumeIdentity,
) -> Result<Option<u64>> {
    if current.disk_number != before.disk_number || current.offset_bytes != before.offset_bytes {
        anyhow::bail!("source volume no longer starts at the authorized current extent");
    }
    if current.extent_length_bytes > before.extent_length_bytes {
        anyhow::bail!("source volume grew while a shrink operation was being evaluated");
    }
    Ok((current.extent_length_bytes < before.extent_length_bytes)
        .then_some(before.extent_length_bytes - current.extent_length_bytes))
}

const VDS_ALIGNMENT_REGISTRY_KEY: &str =
    r"HKEY_LOCAL_MACHINE\System\CurrentControlSet\Services\vds\Alignment";

/// Select the exact VDS registry value documented by Microsoft for `ulAlign == 0`.
/// The defaults are bytes, not booleans or sectors.  Values at and above 4 GiB normally use
/// 1 MiB; the separate names still matter because an administrator may override each size tier.
fn vds_alignment_value_for_disk_size(disk_size_bytes: u64) -> (&'static str, u64) {
    const GIB: u64 = 1024 * 1024 * 1024;
    if disk_size_bytes < 4 * GIB {
        ("LessThan4GB", 64 * 1024)
    } else if disk_size_bytes < 8 * GIB {
        ("Between4_8GB", 1024 * 1024)
    } else if disk_size_bytes <= 32 * GIB {
        ("Between8_32GB", 1024 * 1024)
    } else {
        ("GreaterThan32GB", 1024 * 1024)
    }
}

/// Compute only the extra reversible Shrink needed so that the newly reclaimed tail begins on
/// the same boundary VDS will use when `CreatePartitionEx` receives `ulAlign == 0`.
///
/// This is not a legality check on a provider extent and it never rewrites one: it runs before
/// Shrink creates an extent.  After Shrink, the observed tail offset is passed to VDS unchanged.
/// The payload budget remains exact; any additional bytes are solely the current topology's
/// calculated alignment residue.
fn vds_aligned_reclaim_bytes(
    source: lr_core::windows_storage::VolumeIdentity,
    partition_bytes: u64,
    provider_alignment_bytes: u64,
) -> Result<u64> {
    if provider_alignment_bytes == 0 {
        anyhow::bail!("VDS provider alignment registry value is zero");
    }
    let source_end = source
        .offset_bytes
        .checked_add(source.extent_length_bytes)
        .context("source volume end overflow")?;
    let desired_start = source_end
        .checked_sub(partition_bytes)
        .context("requested staging partition is larger than the source extent")?;
    let alignment_residue = desired_start % provider_alignment_bytes;
    partition_bytes
        .checked_add(alignment_residue)
        .context("VDS-aligned Shrink size overflow")
}

/// Express a byte-capacity minimum as complete sectors of the current physical disk.
///
/// `IVdsVolumeShrink::Shrink` accepts byte counts, while its released extent is represented in
/// complete file-system clusters and therefore complete disk sectors. Passing a non-sector byte
/// minimum can otherwise leave the canonical extent readback a few bytes below the caller's real
/// capacity requirement on providers that truncate the partition extent. The sector size comes
/// from `IOCTL_STORAGE_QUERY_PROPERTY`; no fixed 512/4096 assumption is permitted here.
fn logical_sector_capacity_ceiling(bytes: u64, logical_sector_bytes: u32) -> Result<u64> {
    let sector = u64::from(logical_sector_bytes);
    if sector == 0 {
        anyhow::bail!("current disk reported a zero logical sector size");
    }
    let remainder = bytes % sector;
    if remainder == 0 {
        return Ok(bytes);
    }
    bytes
        .checked_add(sector - remainder)
        .context("sector-rounded Shrink capacity overflow")
}

/// A VDS Shrink may already be committed when a later refresh or identity readback reports an
/// error.  The shared call therefore cannot be treated as proof that nothing changed.  Before the
/// installation has crossed its destructive boundary, inspect the same current volume once and
/// restore only an observed tail shrink.  Never guess from QueryMaxReclaimableBytes or the async
/// reporting value, and never extend a different current extent.
fn rollback_observed_shrink_after_error(
    source_letter: char,
    before: lr_core::windows_storage::VolumeIdentity,
) -> String {
    let current = match lr_core::windows_storage::volume_identity(source_letter) {
        Ok(current) => current,
        Err(error) => {
            return format!(
                "could not read the current source extent after the Shrink error; no blind rollback was attempted: {error}"
            )
        }
    };
    let reclaimed = match observed_shrink_bytes(before, current) {
        Ok(None) => return "the current source extent is unchanged".to_owned(),
        Ok(Some(reclaimed)) => reclaimed,
        Err(error) => {
            return format!(
                "the current source extent is not a safe observed tail shrink; no blind rollback was attempted: {error:#}"
            )
        }
    };
    match lr_core::windows_storage::extend_volume_checked(source_letter, current, reclaimed) {
        Ok(()) => match lr_core::windows_storage::volume_identity(source_letter) {
            Ok(restored) if restored == before => {
                format!("observed {reclaimed} committed shrink bytes and restored the original extent")
            }
            Ok(restored) => format!(
                "the observed shrink was extended, but the authoritative readback is not the original extent: {restored:?}"
            ),
            Err(error) => format!(
                "the observed shrink was extended, but its final extent could not be read: {error}"
            ),
        },
        Err(error) => format!(
            "observed {reclaimed} committed shrink bytes, but restoring the original extent failed: {error}"
        ),
    }
}

fn readback_committed_shrink_or_recover(
    source_letter: char,
    before: lr_core::windows_storage::VolumeIdentity,
    minimum_reclaimed_bytes: u64,
    operation: &str,
) -> Result<lr_core::windows_storage::VolumeIdentity> {
    let current = match lr_core::windows_storage::volume_identity(source_letter) {
        Ok(current) => current,
        Err(error) => {
            let recovery = rollback_observed_shrink_after_error(source_letter, before);
            anyhow::bail!(
                "{operation} committed but its source extent could not be read: {error}; recovery: {recovery}"
            );
        }
    };
    let reclaimed = match observed_shrink_bytes(before, current) {
        Ok(Some(reclaimed)) => reclaimed,
        Ok(None) => {
            let recovery = rollback_observed_shrink_after_error(source_letter, before);
            anyhow::bail!(
                "{operation} returned success but the source extent did not shrink; recovery: {recovery}"
            );
        }
        Err(error) => {
            let recovery = rollback_observed_shrink_after_error(source_letter, before);
            anyhow::bail!(
                "{operation} source readback is not the authorized tail shrink: {error:#}; recovery: {recovery}"
            );
        }
    };
    if reclaimed < minimum_reclaimed_bytes {
        let recovery = rollback_observed_shrink_after_error(source_letter, before);
        anyhow::bail!(
            "{operation} reclaimed {reclaimed} bytes, below the {minimum_reclaimed_bytes}-byte minimum; recovery: {recovery}"
        );
    }
    Ok(current)
}

/// Return only the exact tail proven by the same-volume Shrink readback.
///
/// Do not insert a separate VDS free-extent preflight here. `create_partition_checked_in_envelope`
/// refreshes VDS, intersects the current raw and provider-default extents, executes the real API,
/// and verifies the canonical layout delta. A second query before that boundary can observe a
/// stale provider cache after a committed Shrink and falsely strand this still-reversible task.
fn reclaimed_tail(
    before: lr_core::windows_storage::VolumeIdentity,
    after: lr_core::windows_storage::VolumeIdentity,
) -> Result<lr_core::windows_storage::FreeExtent> {
    let reclaimed = observed_shrink_bytes(before, after)?
        .context("source did not have an observed tail shrink")?;
    let tail_start = after
        .offset_bytes
        .checked_add(after.extent_length_bytes)
        .context("reclaimed tail start overflow")?;
    let tail_end = before
        .offset_bytes
        .checked_add(before.extent_length_bytes)
        .context("reclaimed tail end overflow")?;
    let length_bytes = tail_end
        .checked_sub(tail_start)
        .context("reclaimed tail length underflow")?;
    if length_bytes != reclaimed {
        anyhow::bail!(
            "reclaimed tail arithmetic differs from the authoritative volume length delta"
        );
    }
    Ok(lr_core::windows_storage::FreeExtent {
        offset_bytes: tail_start,
        length_bytes,
    })
}

fn validate_created_in_reclaimed_tail(
    before: lr_core::windows_storage::VolumeIdentity,
    after: lr_core::windows_storage::VolumeIdentity,
    created: lr_core::windows_storage::CreatedPartition,
) -> Result<()> {
    let tail = reclaimed_tail(before, after)?;
    let tail_end = tail
        .offset_bytes
        .checked_add(tail.length_bytes)
        .context("reclaimed tail end overflow")?;
    let created_end = created
        .offset_bytes
        .checked_add(created.size_bytes)
        .context("provider-created partition end overflow")?;
    if created.size_bytes == 0 || created.offset_bytes < tail.offset_bytes || created_end > tail_end
    {
        anyhow::bail!("provider-created partition is outside this Shrink's reclaimed tail");
    }
    Ok(())
}

fn validate_formatted_payload_capacity(
    free_bytes: u64,
    total_bytes: u64,
    payload_bytes: u64,
) -> Result<()> {
    if free_bytes < payload_bytes || total_bytes < payload_bytes {
        anyhow::bail!(
            "formatted volume has {free_bytes} free bytes ({total_bytes} total), below the {payload_bytes}-byte payload"
        );
    }
    Ok(())
}

fn payload_minimum_from_staging_budget(budget_bytes: u64) -> u64 {
    let payload = budget_bytes
        .checked_sub(lr_core::data_staging::STAGING_OPERATIONAL_HEADROOM_BYTES)
        .unwrap_or(budget_bytes);
    if payload == 0 {
        budget_bytes
    } else {
        payload
    }
}

struct ShrinkAndCreateMarkerRequest {
    source_letter: char,
    desired_size_mb: u64,
    payload_size_bytes: u64,
    expected_source_identity: lr_core::windows_storage::VolumeIdentity,
    expected_disk_number: u32,
    expected_partition_number: u32,
    expected_bitlocker_status: VolumeStatus,
    is_current_system: bool,
}

pub struct DiskManager;

impl DiskManager {
    /// Prepare the dual-boot target entirely in normal Windows. A retry reuses the exact adjacent
    /// target/data extents and never shrinks the source a second time.
    pub fn prepare_dual_boot_target(
        plan: &lr_core::custom_install::DualBootPlan,
    ) -> Result<PreparedDualBootTransaction> {
        lr_core::custom_install::validate_dual_boot_plan(plan)
            .map_err(|error| anyhow::anyhow!("invalid dual-boot plan: {error}"))?;
        let source_letter = plan.source_drive_letter.to_ascii_uppercase();
        let source = lr_core::windows_storage::volume_identity(source_letter)
            .map_err(anyhow::Error::from)
            .context("read dual-boot source extent")?;
        if source.offset_bytes != plan.source_offset_bytes {
            anyhow::bail!("dual-boot source volume no longer starts at the planned offset");
        }
        let requested = plan
            .target_length_bytes
            .checked_add(plan.data_length_bytes)
            .context("dual-boot requested size overflow")?;
        let records = lr_core::windows_storage::partitions(source.disk_number)
            .map_err(anyhow::Error::from)
            .context("read dual-boot disk layout")?;
        let target_exists = records
            .iter()
            .filter(|record| {
                record.offset_bytes == plan.target_offset_bytes
                    && record.size_bytes == plan.target_length_bytes
                    && record.kind == lr_core::windows_storage::PartitionKind::BasicData
            })
            .count();
        let data_exists = plan.data_offset_bytes.map_or(0, |offset| {
            records
                .iter()
                .filter(|record| {
                    record.offset_bytes == offset
                        && record.size_bytes == plan.data_length_bytes
                        && record.kind == lr_core::windows_storage::PartitionKind::BasicData
                })
                .count()
        });
        let expected_data = usize::from(plan.data_offset_bytes.is_some());
        if source.extent_length_bytes == plan.source_length_after_bytes
            && target_exists == 1
            && data_exists == expected_data
        {
            let letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
                source.disk_number,
                plan.target_offset_bytes,
            )?;
            // Multiple DOS aliases can legitimately name the same exact current partition.
            // `assigned_drive_letters_for_partition` already binds every returned letter to this
            // disk/offset, so requiring a single alias adds no wrong-volume protection.
            let letter = letters
                .first()
                .copied()
                .context("pre-created dual-boot target has no drive-letter access path")?;
            return Ok(PreparedDualBootTransaction {
                plan: plan.clone(),
                source_disk_number: source.disk_number,
                target_letter: letter,
                data_letter: if let Some(offset) = plan.data_offset_bytes {
                    let letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
                        source.disk_number,
                        offset,
                    )?;
                    Some(
                        *letters
                            .first()
                            .context("pre-created dual-boot data volume has no drive letter")?,
                    )
                } else {
                    None
                },
                created_by_attempt: false,
                active: false,
            });
        }
        if source.extent_length_bytes != plan.source_length_before_bytes
            || target_exists != 0
            || data_exists != 0
        {
            anyhow::bail!(
                "dual-boot source or adjacent target area is partially prepared; refusing a second shrink"
            );
        }

        // Reserve access aliases before the first write. Failure to obtain a letter must not leave
        // the source volume shrunk with no transaction guard available to restore it.
        let target_letter = first_free_drive_letter()?;
        let data_letter = if plan.data_offset_bytes.is_some() {
            Some(first_free_drive_letter_excluding(target_letter)?)
        } else {
            None
        };

        let sector_geometry =
            lr_core::windows_storage::physical_disk_sector_geometry(source.disk_number)
                .map_err(anyhow::Error::from)
                .context("read dual-boot source disk sector geometry")?;
        let shrink_request =
            logical_sector_capacity_ceiling(requested, sector_geometry.logical_sector_bytes)?;
        log::info!(
            "dual-boot Shrink capacity plan: requested={} logical_sector={} provider_request={} overhead={}",
            requested,
            sector_geometry.logical_sector_bytes,
            shrink_request,
            shrink_request - requested
        );
        let _provider_reported_reclaimed = match lr_core::windows_storage::shrink_volume_checked(
            source_letter,
            source,
            shrink_request,
            shrink_request,
        ) {
            Ok(reclaimed) => reclaimed,
            Err(error) => {
                let rollback = rollback_observed_shrink_after_error(source_letter, source);
                anyhow::bail!(
                    "{}; post-error source recovery: {}",
                    crate::core::custom_install_plan::annotate_shrink_error(&error),
                    rollback
                );
            }
        };
        let shrunk = readback_committed_shrink_or_recover(
            source_letter,
            source,
            shrink_request,
            "dual-boot Shrink",
        )?;

        // From this point every fallible provider query must be covered by the move-only rollback
        // guard. In particular QueryFreeExtents can fail after Shrink has already committed; a
        // bare `?` here must not strand the source at its smaller length.
        let mut transaction = PreparedDualBootTransaction {
            plan: lr_core::custom_install::DualBootPlan {
                source_length_after_bytes: shrunk.extent_length_bytes,
                ..plan.clone()
            },
            source_disk_number: source.disk_number,
            target_letter,
            data_letter,
            created_by_attempt: true,
            active: true,
        };

        let data_payload_minimum = plan
            .data_offset_bytes
            .map(|_| payload_minimum_from_staging_budget(plan.data_length_bytes))
            .unwrap_or(0);
        let combined_minimum = plan
            .target_length_bytes
            .checked_add(data_payload_minimum)
            .context("dual-boot combined minimum overflow")?;
        let tail = reclaimed_tail(source, shrunk)?;
        if tail.length_bytes < combined_minimum {
            anyhow::bail!(
                "observed dual-boot Shrink tail has {} bytes, below the {}-byte combined minimum",
                tail.length_bytes,
                combined_minimum
            );
        }
        transaction.plan.target_offset_bytes = tail.offset_bytes;
        let target_envelope = lr_core::windows_storage::FreeExtent {
            offset_bytes: tail.offset_bytes,
            // Keep the optional data payload minimum outside the Windows authorization envelope.
            // A later provider-selected Windows start may consume alignment slack, but must not
            // consume bytes already authorized for the staged payload.
            length_bytes: tail
                .length_bytes
                .checked_sub(data_payload_minimum)
                .context("dual-boot Windows authorization envelope underflow")?,
        };
        let target_snapshot = lr_core::windows_storage::disk_layout_snapshot(source.disk_number)?;
        let target = lr_core::windows_storage::create_partition_checked_in_envelope(
            &lr_core::windows_storage::CreatePartitionRequest {
                disk_number: source.disk_number,
                offset_bytes: tail.offset_bytes,
                // The confirmation dialog names this exact Windows capacity.  Even when VDS
                // reclaims a slightly larger provider extent, do not silently turn the Windows
                // volume into "all remaining space".  The checked create boundary may still
                // return a legal provider-rounded extent, which is recorded below from readback.
                size_bytes: plan.target_length_bytes,
                kind: lr_core::windows_storage::PartitionKind::BasicData,
                file_system: Some(lr_core::windows_storage::FileSystem::Ntfs),
                label: "Windows".to_owned(),
                drive_letter: Some(transaction.target_letter),
                active: false,
                preserve_gpt_metadata: None,
            },
            target_envelope,
            plan.target_length_bytes,
            &target_snapshot,
        );
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                let rollback = transaction.rollback_inner();
                transaction.active = false;
                return Err(anyhow::anyhow!(
                    "create dual-boot Windows volume failed: {error}; rollback: {}",
                    rollback
                        .map(|_| "succeeded".to_owned())
                        .unwrap_or_else(|value| format!("failed: {value:#}"))
                ));
            }
        };
        transaction.plan.target_offset_bytes = target.offset_bytes;
        transaction.plan.target_length_bytes = target.size_bytes;
        validate_created_in_reclaimed_tail(source, shrunk, target)?;

        if plan.data_offset_bytes.is_some() {
            let data_letter = data_letter.expect("dual-boot data letter was reserved before write");
            let target_end = target
                .offset_bytes
                .checked_add(target.size_bytes)
                .context("dual-boot target end overflow")?;
            let tail_end = tail
                .offset_bytes
                .checked_add(tail.length_bytes)
                .context("dual-boot reclaimed tail end overflow")?;
            let data_length = tail_end
                .checked_sub(target_end)
                .context("provider-created Windows volume escaped the reclaimed tail")?;
            if data_length < data_payload_minimum {
                anyhow::bail!(
                    "remaining dual-boot data envelope has {data_length} bytes, below the {data_payload_minimum}-byte payload minimum"
                );
            }
            let data_extent = lr_core::windows_storage::FreeExtent {
                offset_bytes: target_end,
                length_bytes: data_length,
            };
            let data_create_bytes = plan.data_length_bytes.min(data_extent.length_bytes);
            let snapshot = lr_core::windows_storage::disk_layout_snapshot(source.disk_number)?;
            let data = lr_core::windows_storage::create_partition_checked_in_envelope(
                &lr_core::windows_storage::CreatePartitionRequest {
                    disk_number: source.disk_number,
                    offset_bytes: data_extent.offset_bytes,
                    // The staging budget is the exact manifest/file total plus one fixed 2 GiB
                    // allowance.  Do not consume an arbitrarily larger provider extent merely
                    // because Shrink reclaimed more than its requested minimum.
                    size_bytes: data_create_bytes,
                    kind: lr_core::windows_storage::PartitionKind::BasicData,
                    file_system: Some(lr_core::windows_storage::FileSystem::Ntfs),
                    label: "Data".to_owned(),
                    drive_letter: Some(data_letter),
                    active: false,
                    preserve_gpt_metadata: None,
                },
                data_extent,
                data_payload_minimum,
                &snapshot,
            );
            let data = match data {
                Ok(data) => data,
                Err(error) => {
                    let rollback = transaction.rollback_inner();
                    transaction.active = false;
                    return Err(anyhow::anyhow!(
                        "create optional dual-boot data volume failed: {error}; rollback: {}",
                        rollback
                            .map(|_| "succeeded".to_owned())
                            .unwrap_or_else(|value| format!("failed: {value:#}"))
                    ));
                }
            };
            transaction.plan.data_offset_bytes = Some(data.offset_bytes);
            transaction.plan.data_length_bytes = data.size_bytes;
            validate_created_in_reclaimed_tail(source, shrunk, data)?;
            let (data_free_bytes, data_total_bytes) = Self::get_volume_space_bytes(data_letter)
                .context("could not read formatted dual-boot staging volume capacity")?;
            validate_formatted_payload_capacity(
                data_free_bytes,
                data_total_bytes,
                data_payload_minimum,
            )?;
        }
        lr_core::custom_install::validate_dual_boot_plan(&transaction.plan).map_err(|error| {
            anyhow::anyhow!("invalid provider-created dual-boot layout: {error}")
        })?;
        Ok(transaction)
    }
    /// Read-only current-session attachment classification used by the explicit full-disk UI.
    pub fn storage_attachment(disk_number: u32) -> StorageAttachment {
        Self::get_storage_profile(disk_number).1
    }

    /// 获取所有固定磁盘分区列表
    pub fn get_partitions() -> Result<Vec<Partition>> {
        let mut partitions = Vec::new();
        let is_pe = Self::is_pe_environment();
        let running_windows_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;

        // 预先创建 BitLockerManager 实例，避免重复创建
        let bitlocker_manager = BitLockerManager::new();

        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            if let Ok(info) =
                Self::get_partition_info(&drive, is_pe, running_windows_drive, &bitlocker_manager)
            {
                partitions.push(info);
            }
        }

        // Disk and partition numbers can be reused after a hot-plug or a
        // delete/recreate cycle. Enrich every visible volume with immutable
        // geometry from the physical disk before any install intent is built.
        let mut disk_geometry = std::collections::BTreeMap::new();
        let mut canonical_layouts = std::collections::BTreeMap::new();
        let mut disk_profiles = std::collections::BTreeMap::new();
        for partition in &partitions {
            if let Some(disk_number) = partition.disk_number {
                disk_geometry.entry(disk_number).or_insert_with(|| {
                    crate::core::quick_partition::get_physical_disk(disk_number)
                });
                canonical_layouts
                    .entry(disk_number)
                    .or_insert_with(|| lr_core::windows_storage::disk_layout_snapshot(disk_number));
                disk_profiles
                    .entry(disk_number)
                    .or_insert_with(|| Self::get_storage_profile(disk_number));
            }
        }
        for partition in &mut partitions {
            let (Some(disk_number), Some(partition_number)) =
                (partition.disk_number, partition.partition_number)
            else {
                continue;
            };
            let Some(Some(disk)) = disk_geometry.get(&disk_number) else {
                continue;
            };
            let Some(physical_partition) = disk
                .partitions
                .iter()
                .find(|candidate| candidate.partition_number == partition_number)
            else {
                continue;
            };
            partition.disk_size_bytes = Some(disk.size_bytes);
            partition.partition_offset_bytes = Some(physical_partition.offset_bytes);
            partition.partition_size_bytes = Some(physical_partition.size_bytes);
            partition.partition_kind = Some(if physical_partition.is_esp {
                lr_core::windows_storage::PartitionKind::EfiSystem
            } else if physical_partition.is_msr {
                lr_core::windows_storage::PartitionKind::MicrosoftReserved
            } else if physical_partition.is_recovery {
                lr_core::windows_storage::PartitionKind::Recovery
            } else {
                lr_core::windows_storage::PartitionKind::BasicData
            });
            partition.install_target_eligible = partition.partition_kind
                == Some(lr_core::windows_storage::PartitionKind::BasicData)
                && canonical_layouts
                    .get(&disk_number)
                    .and_then(|result| result.as_ref().ok())
                    .and_then(|layout| {
                        layout.partitions.iter().find(|candidate| {
                            candidate.offset_bytes == physical_partition.offset_bytes
                                && candidate.size_bytes == physical_partition.size_bytes
                        })
                    })
                    .is_some_and(|candidate| {
                        lr_core::windows_storage::partition_token_is_installable_user_data(
                            candidate.token,
                        )
                    });
            partition.storage_media = disk_profiles
                .get(&disk_number)
                .map_or(StorageMedia::Unknown, |profile| profile.0);
        }

        Ok(partitions)
    }

    /// Explorer-like install inventory. Hidden OEM/service/recovery partitions may temporarily
    /// acquire letters in WinPE, but are not user-selectable Windows targets.
    pub fn get_install_partitions() -> Result<Vec<Partition>> {
        let mut partitions = Self::get_partitions()?;
        partitions.retain(|partition| partition.install_target_eligible);
        Ok(partitions)
    }

    fn get_partition_info(
        drive: &str,
        is_pe: bool,
        running_windows_drive: char,
        bitlocker_manager: &BitLockerManager,
    ) -> Result<Partition> {
        Self::get_partition_info_for_staging(
            drive,
            is_pe,
            running_windows_drive,
            bitlocker_manager,
            false,
        )
        .map(|(partition, _)| partition)
    }

    fn get_partition_info_for_staging(
        drive: &str,
        is_pe: bool,
        running_windows_drive: char,
        bitlocker_manager: &BitLockerManager,
        include_removable: bool,
    ) -> Result<(Partition, StagingDriveKind)> {
        let path = format!("{}\\", drive);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let drive_kind = {
            #[cfg(windows)]
            {
                // 光驱、虚拟光驱、网络盘、RAM 盘和无法识别的卷不允许承载安装数据。
                // USB 移动 SSD 可能被 Windows 报为 DRIVE_FIXED，后续还会结合总线类型降级。
                let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };
                let kind = classify_staging_drive_type(drive_type)
                    .ok_or_else(|| anyhow::anyhow!("Unsupported staging drive type"))?;
                if kind == StagingDriveKind::Removable && !include_removable {
                    anyhow::bail!("Not a fixed drive");
                }
                kind
            }
            #[cfg(not(windows))]
            {
                StagingDriveKind::Fixed
            }
        };

        // 获取磁盘空间
        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        #[cfg(windows)]
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available as *mut u64),
                Some(&mut total_bytes as *mut u64),
                Some(&mut total_free_bytes as *mut u64),
            )?;
        }

        // 获取卷标
        let mut volume_name = [0u16; 261];
        #[cfg(windows)]
        unsafe {
            let _ = GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut volume_name),
                None,
                None,
                None,
                None,
            );
        }
        let label = String::from_utf16_lossy(&volume_name)
            .trim_end_matches('\0')
            .to_string();

        // 检查是否为当前系统分区
        let is_current_system = drive
            .chars()
            .next()
            .is_some_and(|letter| letter.eq_ignore_ascii_case(&running_windows_drive));

        // 检查是否包含 Windows 系统
        let windows_path = format!("{}\\Windows\\System32", drive);
        let has_windows = Path::new(&windows_path).exists();

        // 在 PE 环境下，is_system_partition 表示是否包含 Windows
        // 在正常环境下，is_system_partition 表示是否是当前系统盘
        let is_system_partition = if is_pe {
            has_windows && !is_current_system // PE下排除 X: 盘
        } else {
            is_current_system
        };

        // 获取分区表类型、磁盘号和分区号
        let detail = Self::get_partition_style(drive);

        // 获取 BitLocker 状态
        let letter_char = drive.chars().next().unwrap_or('C');
        let bitlocker_status = bitlocker_manager.get_status(letter_char);
        let stable_identity = match lr_core::windows_storage::stable_volume_identity(letter_char) {
            Ok(identity) => Some(identity),
            Err(error) => {
                log::warn!("[DISK INVENTORY] cannot capture stable identity for {drive}: {error}");
                None
            }
        };

        Ok((
            Partition {
                letter: drive.to_string(),
                total_size_mb: total_bytes / 1024 / 1024,
                free_size_mb: free_bytes_available / 1024 / 1024,
                free_size_bytes: free_bytes_available,
                label,
                is_system_partition,
                has_windows,
                partition_style: detail.style,
                disk_number: detail.disk_number,
                partition_number: detail.partition_number,
                disk_size_bytes: None,
                partition_offset_bytes: None,
                partition_size_bytes: None,
                partition_kind: None,
                install_target_eligible: false,
                storage_media: StorageMedia::Unknown,
                stable_identity,
                bitlocker_status,
            },
            drive_kind,
        ))
    }

    /// 使用 Windows API 获取分区表类型和分区号 (GPT/MBR)
    #[cfg(windows)]
    fn get_partition_style(drive: &str) -> PartitionDetail {
        let letter = drive.chars().next().unwrap_or('C');

        // 先获取磁盘号和分区号
        let (disk_number, partition_number) = Self::get_device_number(letter);

        // 再获取分区表类型
        let style = if let Some(disk_num) = disk_number {
            Self::get_disk_partition_style_api(disk_num)
        } else {
            PartitionStyle::Unknown
        };

        PartitionDetail {
            style,
            disk_number,
            partition_number,
        }
    }

    #[cfg(not(windows))]
    fn get_partition_style(_drive: &str) -> PartitionDetail {
        PartitionDetail {
            style: PartitionStyle::Unknown,
            disk_number: None,
            partition_number: None,
        }
    }

    /// 使用 IOCTL_STORAGE_GET_DEVICE_NUMBER 获取磁盘号和分区号
    #[cfg(windows)]
    fn get_device_number(letter: char) -> (Option<u32>, Option<u32>) {
        unsafe {
            // 打开卷设备
            let volume_path = format!("\\\\.\\{}:", letter);
            let wide_path: Vec<u16> = volume_path
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0, // 不需要读写权限，只需要查询
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return (None, None),
            };

            if handle == INVALID_HANDLE_VALUE {
                return (None, None);
            }

            let mut device_number = STORAGE_DEVICE_NUMBER::default();
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                None,
                0,
                Some(&mut device_number as *mut _ as *mut _),
                std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            result
                .is_ok()
                .then(|| {
                    validated_storage_device_number(
                        bytes_returned,
                        device_number.DeviceNumber,
                        device_number.PartitionNumber,
                    )
                })
                .flatten()
                .map_or((None, None), |(disk, partition)| {
                    (Some(disk), Some(partition))
                })
        }
    }

    #[cfg(windows)]
    fn get_storage_profile(disk_number: u32) -> (StorageMedia, StorageAttachment) {
        unsafe {
            let disk_path = format!("\\\\.\\PhysicalDrive{disk_number}");
            let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();
            let handle = match CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            ) {
                Ok(handle) if handle != INVALID_HANDLE_VALUE => handle,
                _ => return (StorageMedia::Unknown, StorageAttachment::Unknown),
            };

            let mut query = STORAGE_PROPERTY_QUERY {
                PropertyId: StorageDeviceSeekPenaltyProperty,
                QueryType: PropertyStandardQuery,
                AdditionalParameters: [0],
            };
            let mut seek = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
            let mut bytes_returned = 0;
            let seek_result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const std::ffi::c_void),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(&mut seek as *mut _ as *mut std::ffi::c_void),
                std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                Some(&mut bytes_returned),
                None,
            );
            // Microsoft documents this property for Windows 7+. Unsupported or malformed
            // descriptors remain Unknown; they never get guessed as either HDD or SSD.
            let seek_penalty = if seek_result.is_ok() {
                validated_seek_penalty_descriptor(
                    seek.Version,
                    seek.Size,
                    bytes_returned,
                    seek.IncursSeekPenalty.0 != 0,
                )
            } else {
                None
            };

            query.PropertyId = StorageDeviceProperty;
            let mut descriptor = STORAGE_DEVICE_DESCRIPTOR::default();
            bytes_returned = 0;
            let descriptor_result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const std::ffi::c_void),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(&mut descriptor as *mut _ as *mut std::ffi::c_void),
                std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32,
                Some(&mut bytes_returned),
                None,
            );
            let _ = CloseHandle(handle);

            let bus_type = if descriptor_result.is_ok() {
                validated_storage_bus_type(
                    descriptor.Version,
                    descriptor.Size,
                    bytes_returned,
                    descriptor.BusType.0,
                )
            } else {
                None
            };
            let attachment = match bus_type {
                Some(value)
                    if [BusType1394.0, BusTypeUsb.0, BusTypeSd.0, BusTypeMmc.0]
                        .contains(&value) =>
                {
                    StorageAttachment::External
                }
                Some(value) if [BusTypeVirtual.0, BusTypeFileBackedVirtual.0].contains(&value) => {
                    StorageAttachment::Unknown
                }
                None => StorageAttachment::Unknown,
                Some(_) => StorageAttachment::Internal,
            };
            let media = match seek_penalty {
                Some(true) => StorageMedia::Rotational,
                Some(false) => StorageMedia::SolidState,
                None if bus_type
                    .is_some_and(|value| [BusTypeNvme.0, BusTypeSCM.0].contains(&value)) =>
                {
                    StorageMedia::SolidState
                }
                None => StorageMedia::Unknown,
            };
            (media, attachment)
        }
    }

    #[cfg(not(windows))]
    fn get_storage_profile(_disk_number: u32) -> (StorageMedia, StorageAttachment) {
        (StorageMedia::Unknown, StorageAttachment::Unknown)
    }

    #[cfg(windows)]
    fn get_volume_space_bytes(letter: char) -> Option<(u64, u64)> {
        let path = format!("{}:\\", letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) } == DRIVE_CDROM {
            return None;
        }
        let mut free_bytes_available = 0;
        let mut total_bytes = 0;
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide_path.as_ptr()),
                Some(&mut free_bytes_available),
                Some(&mut total_bytes),
                None,
            )
            .ok()
            .map(|_| (free_bytes_available, total_bytes))
        }
    }

    #[cfg(windows)]
    fn volume_is_writable(letter: char) -> bool {
        const FILE_READ_ONLY_VOLUME_FLAG: u32 = 0x0008_0000;

        let path = format!("{}:\\", letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut file_system_flags = 0u32;
        unsafe {
            GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                None,
                None,
                None,
                Some(&mut file_system_flags),
                None,
            )
            .is_ok()
                && file_system_flags & FILE_READ_ONLY_VOLUME_FLAG == 0
        }
    }

    #[cfg(not(windows))]
    fn volume_is_writable(_letter: char) -> bool {
        true
    }

    fn get_staging_partitions() -> Result<Vec<(Partition, StagingDriveKind)>> {
        let mut partitions = Vec::new();
        let is_pe = Self::is_pe_environment();
        let running_windows_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;
        let bitlocker_manager = BitLockerManager::new();

        for letter in b'A'..=b'Z' {
            let letter = letter as char;
            let drive = format!("{letter}:");
            let Ok((partition, drive_kind)) = Self::get_partition_info_for_staging(
                &drive,
                is_pe,
                running_windows_drive,
                &bitlocker_manager,
                true,
            ) else {
                continue;
            };
            if !Self::volume_is_writable(letter) {
                log::warn!(
                    "[DISK] 跳过 {}:：卷只读或无法确认可写，不能承载 ViaPE 安装数据",
                    letter
                );
                continue;
            }
            partitions.push((partition, drive_kind));
        }

        Ok(partitions)
    }

    #[cfg(not(windows))]
    fn get_volume_space_bytes(_letter: char) -> Option<(u64, u64)> {
        None
    }

    #[cfg(windows)]
    fn get_volume_file_system(letter: char) -> Option<String> {
        let path = format!("{}:\\", letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut file_system_name = [0u16; 64];
        unsafe {
            GetVolumeInformationW(
                PCWSTR(wide_path.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut file_system_name),
            )
            .ok()?;
        }
        Some(
            String::from_utf16_lossy(&file_system_name)
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    #[cfg(not(windows))]
    fn get_volume_file_system(_letter: char) -> Option<String> {
        None
    }

    /// 使用 IOCTL_DISK_GET_DRIVE_LAYOUT_EX 获取磁盘分区表类型
    #[cfg(windows)]
    fn get_disk_partition_style_api(disk_number: u32) -> PartitionStyle {
        match lr_core::windows_storage::disk_style(disk_number) {
            Ok(lr_core::windows_storage::DiskStyle::Mbr) => PartitionStyle::MBR,
            Ok(lr_core::windows_storage::DiskStyle::Gpt) => PartitionStyle::GPT,
            Err(error) => {
                log::warn!("读取磁盘 {disk_number} 分区表样式失败: {error}");
                PartitionStyle::Unknown
            }
        }
    }

    /// 格式化指定分区
    pub fn format_partition(partition: &str) -> Result<String> {
        lr_core::format_command::FormatCommandSpec::new(partition, "NTFS", None)
            .map_err(|error| anyhow::anyhow!("无效的格式化参数: {error}"))?;
        let drive_letter = partition
            .trim()
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("格式化目标缺少盘符"))?;
        let expected = lr_core::windows_storage::stable_volume_identity(drive_letter)
            .map_err(|error| anyhow::anyhow!("无法确认格式化目标的稳定身份: {error}"))?;
        lr_core::windows_storage::format_drive_with_options_stable_checked(
            drive_letter,
            expected,
            &lr_core::windows_storage::FormatOptions {
                file_system: lr_core::windows_storage::FileSystem::Ntfs,
                label: String::new(),
                allocation_unit_size: 0,
                quick: true,
                force_dismount: false,
            },
        )
        .map_err(|error| anyhow::anyhow!("格式化分区失败: {error}"))?;
        Ok("format completed".to_owned())
    }

    /// 从指定分区缩小并创建新分区
    fn shrink_and_create_partition(
        source_partition: &str,
        new_letter: &str,
        size_mb: u64,
        payload_size_bytes: u64,
    ) -> Result<PreparedStagingTransaction> {
        let source = source_partition
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("源分区盘符为空"))?
            .to_ascii_uppercase();
        let target = new_letter
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("新分区盘符为空"))?
            .to_ascii_uppercase();
        let source_identity = lr_core::windows_storage::volume_identity(source)?;
        let disk_number = source_identity.disk_number;
        let bytes = size_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| anyhow::anyhow!("分区大小超出支持范围"))?;
        if payload_size_bytes == 0 || payload_size_bytes > bytes {
            anyhow::bail!(
                "staging payload must be non-zero and no larger than the requested partition budget"
            );
        }
        let required_partition_bytes = required_staging_bytes(payload_size_bytes)
            .context("staging payload plus fixed 2 GiB headroom overflow")?;
        if required_partition_bytes > bytes {
            anyhow::bail!(
                "requested staging partition has {bytes} bytes, below the exact payload plus fixed 2 GiB requirement {required_partition_bytes}"
            );
        }
        let disk_layout = lr_core::windows_storage::disk_layout_snapshot(disk_number)
            .context("failed to read the current disk size for VDS alignment planning")?;
        let (alignment_value_name, documented_default_alignment) =
            vds_alignment_value_for_disk_size(disk_layout.disk_size_bytes);
        let provider_alignment = lr_core::registry::OfflineRegistry::query_dword_optional(
            VDS_ALIGNMENT_REGISTRY_KEY,
            alignment_value_name,
        )
        .with_context(|| {
            format!("failed to read documented VDS alignment value {alignment_value_name}")
        })?
        .map(u64::from)
        .unwrap_or(documented_default_alignment);
        let shrink_bytes = vds_aligned_reclaim_bytes(source_identity, bytes, provider_alignment)?;
        log::info!(
            "VDS staging alignment plan: disk={} disk_size={} value={} alignment={} partition_budget={} shrink_bytes={} topology_overhead={}",
            disk_number,
            disk_layout.disk_size_bytes,
            alignment_value_name,
            provider_alignment,
            bytes,
            shrink_bytes,
            shrink_bytes - bytes
        );
        let _provider_reported = match lr_core::windows_storage::shrink_volume_checked(
            source,
            source_identity,
            shrink_bytes,
            shrink_bytes,
        ) {
            Ok(reclaimed) => reclaimed,
            Err(error) => {
                let rollback = rollback_observed_shrink_after_error(source, source_identity);
                anyhow::bail!(
                    "shrink staging source failed: {error}; post-error source recovery: {rollback}"
                );
            }
        };
        let shrunk = readback_committed_shrink_or_recover(
            source,
            source_identity,
            shrink_bytes,
            "staging Shrink",
        )?;
        let mut transaction = PreparedStagingTransaction {
            source_letter: source,
            source_before: source_identity,
            source_after: shrunk,
            created: None,
            target_letter: target,
            active: true,
        };
        let tail = reclaimed_tail(source_identity, shrunk)?;
        if tail.length_bytes < required_partition_bytes {
            anyhow::bail!(
                "observed staging Shrink tail has {} bytes, below the {}-byte payload plus fixed-headroom minimum",
                tail.length_bytes,
                required_partition_bytes
            );
        }
        let create_bytes = bytes.min(tail.length_bytes);
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
        let created = lr_core::windows_storage::create_partition_checked_in_envelope(
            &lr_core::windows_storage::CreatePartitionRequest {
                disk_number,
                // The same-volume Shrink readback proves this desired start and authorization
                // envelope. The shared boundary refreshes VDS and uses provider-default alignment
                // internally, so a sector-valid but non-MiB result remains acceptable.
                offset_bytes: tail.offset_bytes,
                size_bytes: create_bytes,
                kind: lr_core::windows_storage::PartitionKind::BasicData,
                file_system: Some(lr_core::windows_storage::FileSystem::Ntfs),
                label: String::new(),
                drive_letter: Some(target),
                active: false,
                preserve_gpt_metadata: None,
            },
            tail,
            required_partition_bytes,
            &expected_layout,
        );
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                let rollback = transaction.rollback_inner();
                transaction.active = false;
                return Err(anyhow::anyhow!(
                    "缩小源分区后创建新分区失败: {error}; 回滚扩容结果: {}",
                    rollback
                        .map(|_| "成功".to_string())
                        .unwrap_or_else(|rollback_error| format!("失败: {rollback_error}"))
                ));
            }
        };
        transaction.created = Some(created);
        validate_created_in_reclaimed_tail(source_identity, shrunk, created)?;
        Ok(transaction)
    }

    /// 删除指定分区
    pub fn delete_partition(partition_letter: &str) -> Result<String> {
        let letter = partition_letter
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("分区盘符为空"))?
            .to_ascii_uppercase();
        let (disk_number, partition_number) = Self::get_device_number(letter);
        let disk_number =
            disk_number.ok_or_else(|| anyhow::anyhow!("无法确认分区 {}: 所在磁盘", letter))?;
        let partition_number =
            partition_number.ok_or_else(|| anyhow::anyhow!("无法确认分区 {}: 的分区号", letter))?;
        super::quick_partition::delete_partition(disk_number, partition_number)
    }

    pub fn is_pe_environment() -> bool {
        crate::core::system_info::SystemInfo::check_pe_environment()
    }

    /// 获取所有已使用的盘符
    pub fn get_used_drive_letters() -> Vec<char> {
        let Ok(mask) = lr_core::windows_storage::assigned_drive_letter_mask() else {
            // A failed inventory must not cause an already assigned letter to
            // be reused by a destructive storage operation.
            return ('A'..='Z').collect();
        };
        (0u8..=25)
            .filter(|index| mask & (1u32 << index) != 0)
            .map(|index| char::from(b'A' + index))
            .collect()
    }

    /// 查找第一个可用的盘符（未被使用的）
    pub fn find_available_drive_letter() -> Option<char> {
        let used = Self::get_used_drive_letters();
        // 从后往前找，避开常用盘符
        for letter in ('E'..='Z').rev() {
            if !used.contains(&letter) {
                return Some(letter);
            }
        }
        // 如果都被占用，尝试 D
        if !used.contains(&'D') {
            return Some('D');
        }
        None
    }

    /// 从指定分区缩小并创建新分区（增强版，带标志文件）
    ///
    /// # Arguments
    /// * `source_letter` - 源分区盘符
    /// * `desired_size_mb` - 期望的新分区大小（MB）
    ///
    /// # Returns
    /// * `Ok(PreparedStagingTransaction)` - 持有新分区及源卷实际范围的回滚事务
    /// * `Err` - 错误信息
    fn shrink_and_create_partition_with_marker(
        request: ShrinkAndCreateMarkerRequest,
    ) -> Result<PreparedStagingTransaction> {
        let ShrinkAndCreateMarkerRequest {
            source_letter,
            desired_size_mb,
            payload_size_bytes,
            expected_source_identity,
            expected_disk_number,
            expected_partition_number,
            expected_bitlocker_status,
            is_current_system,
        } = request;
        let source_letter = source_letter.to_ascii_uppercase();
        let current_identity = lr_core::windows_storage::volume_identity(source_letter)?;
        if current_identity != expected_source_identity {
            anyhow::bail!(
                "分区范围已变化，拒绝缩小 {}:：预期磁盘 {} offset {} length {}，当前为 {:?}",
                source_letter,
                expected_source_identity.disk_number,
                expected_source_identity.offset_bytes,
                expected_source_identity.extent_length_bytes,
                current_identity
            );
        }

        let refreshed_bitlocker_status = BitLockerManager::new().get_status(source_letter);
        if refreshed_bitlocker_status != expected_bitlocker_status
            || !matches!(
                refreshed_bitlocker_status,
                VolumeStatus::NotEncrypted | VolumeStatus::EncryptedUnlocked
            )
            || (refreshed_bitlocker_status == VolumeStatus::EncryptedUnlocked && !is_current_system)
        {
            anyhow::bail!(
                "BitLocker 状态已变化或不允许自动缩卷，拒绝缩小 {}:：预期={}，当前={}，当前系统卷={}",
                source_letter,
                expected_bitlocker_status.as_str(),
                refreshed_bitlocker_status.as_str(),
                is_current_system
            );
        }
        if refreshed_bitlocker_status == VolumeStatus::EncryptedUnlocked {
            log::info!(
                "[DISK] {}: 为已解锁且转换状态稳定的 BitLocker 当前系统卷，使用 Windows 原生缩卷而不执行完整解密",
                source_letter
            );
        }

        let actual_size_mb = desired_size_mb;

        // 找一个可用的盘符
        let new_letter = Self::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有可用的盘符")))?;

        log::info!(
            "[DISK] 准备从 {}: 缩小 {} MB 并创建新分区 {}:",
            source_letter,
            actual_size_mb,
            new_letter
        );

        let transaction = Self::shrink_and_create_partition(
            &source_letter.to_string(),
            &new_letter.to_string(),
            actual_size_mb,
            payload_size_bytes,
        )?;
        log::info!(
            "[DISK] WinAPI 已从磁盘 {} 分区 {} 创建暂存卷 {}",
            expected_disk_number,
            expected_partition_number,
            new_letter
        );

        // 等待系统识别新分区
        std::thread::sleep(std::time::Duration::from_secs(2));

        // 验证新分区是否创建成功
        let new_partition_path = format!("{}:\\", new_letter);
        for retry in 0..5 {
            if Path::new(&new_partition_path).exists() {
                break;
            }
            if retry == 4 {
                anyhow::bail!("{}", tr!("分区创建失败：新分区 {}: 不可访问。", new_letter));
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        let _created = transaction
            .created
            .context("staging transaction lost its provider-created extent")?;
        let (free_bytes, total_bytes) = Self::get_volume_space_bytes(new_letter)
            .context("could not read formatted staging volume capacity")?;
        validate_formatted_payload_capacity(free_bytes, total_bytes, payload_size_bytes)?;

        // 写入标志文件
        let marker_path = format!("{}:\\{}", new_letter, AUTO_CREATED_PARTITION_MARKER);
        std::fs::write(
            &marker_path,
            format!(
                "LetRecovery Auto Created Partition\n\
                Created: {}\n\
                Source: {}:\n\
                SourceDisk: {}\n\
                SourcePartition: {}\n\
                Size: {} MB\n\
                Note: This partition was automatically created and can be safely deleted after system installation.",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                source_letter,
                expected_disk_number,
                expected_partition_number,
                actual_size_mb
            ),
        )
        .map_err(|e| anyhow::anyhow!("{}", tr!("写入标志文件失败: {}", e)))?;

        log::info!(
            "[DISK] 新分区 {}: 创建成功，大小 {} MB，标志文件已写入",
            new_letter,
            actual_size_mb
        );

        Ok(transaction)
    }

    /// 查找可用的数据分区（排除指定分区、光驱，检查空间）
    ///
    /// # Arguments
    /// * `exclude_partition` - 要排除的分区（通常是目标安装分区）
    /// * `payload_size_bytes` - 调用端已精确累计、实际会写入数据分区的全部文件逻辑大小；
    ///   共享策略只在这里统一加一次固定 2 GiB
    /// * `allow_shrink_of_excluded_target` - 仅普通重装/全盘暂存可用；双系统源卷必须由
    ///   单一预创建事务精确缩卷，不能先为暂存缩一次、随后再按旧几何缩第二次
    ///
    /// # Returns
    /// * `Ok(Some((partition, transaction)))` - 找到可用分区；自动缩卷时同时返回必须保活到
    ///   PE 交接提交点的事务，现有分区则为 `None`
    /// * `Ok(None)` - 没有找到可用分区，且无法自动创建
    /// * `Err` - 发生错误
    pub(super) fn find_suitable_data_partition(
        exclude_partition: &str,
        payload_size_bytes: u64,
        allow_shrink_of_excluded_target: bool,
        force_target_shrink: bool,
    ) -> Result<Option<(String, Option<PreparedStagingTransaction>)>> {
        let exclude_letter = exclude_partition
            .chars()
            .next()
            .unwrap_or('C')
            .to_ascii_uppercase();

        log::info!(
            "[DISK] 查找数据分区，目标: {}, 全部暂存文件: {} bytes ({:.2} GB)，另加固定 2 GiB 余量",
            exclude_partition,
            payload_size_bytes,
            payload_size_bytes as f64 / 1024.0 / 1024.0 / 1024.0
        );

        let fixed_partitions = Self::get_partitions()?;
        let target = fixed_partitions
            .iter()
            .find(|partition| {
                partition
                    .letter
                    .chars()
                    .next()
                    .is_some_and(|letter| letter.eq_ignore_ascii_case(&exclude_letter))
            })
            .cloned();
        let target_disk_number = target.as_ref().and_then(|partition| partition.disk_number);
        let staging_partitions = Self::get_staging_partitions()?;

        let mut profiles = std::collections::HashMap::new();
        let mut candidates = Vec::new();
        for (partition, drive_kind) in &staging_partitions {
            let Some(letter) = partition
                .letter
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
            else {
                continue;
            };
            if letter == exclude_letter || letter == 'X' {
                continue;
            }
            if partition.bitlocker_status != VolumeStatus::NotEncrypted {
                log::warn!(
                    "[DISK] 跳过 {}:：BitLocker 状态为 {}，重启到 PE 后不能保证可访问",
                    letter,
                    partition.bitlocker_status.as_str()
                );
                continue;
            }
            let (media, detected_attachment) = partition
                .disk_number
                .map(|disk_number| {
                    *profiles
                        .entry(disk_number)
                        .or_insert_with(|| Self::get_storage_profile(disk_number))
                })
                .unwrap_or((StorageMedia::Unknown, StorageAttachment::Unknown));
            let attachment = if *drive_kind == StagingDriveKind::Removable {
                StorageAttachment::External
            } else {
                detected_attachment
            };
            let candidate = StagingCandidate {
                letter,
                disk_number: partition.disk_number,
                media,
                attachment,
                free_bytes: partition.free_size_bytes,
            };
            log::info!(
                "[DISK] 暂存候选 {}: 磁盘={:?} 介质={:?} 接口={:?} 剩余={:.2} GB",
                letter,
                candidate.disk_number,
                candidate.media,
                candidate.attachment,
                candidate.free_bytes as f64 / 1024.0 / 1024.0 / 1024.0
            );
            candidates.push(candidate);
        }

        let selection_candidates: &[StagingCandidate] = if force_target_shrink {
            log::warn!(
                "[CI AUTOMATION] session-bound run is forcing the normal auto-staging shrink fallback"
            );
            &[]
        } else {
            &candidates
        };
        let initial_plan = select_staging_plan(
            payload_size_bytes,
            target_disk_number,
            selection_candidates,
            None,
        );
        let initial_plan_uses_external = match initial_plan {
            StagingPlan::Existing { letter, .. } => candidates
                .iter()
                .find(|candidate| candidate.letter.eq_ignore_ascii_case(&letter))
                .is_some_and(|candidate| candidate.attachment == StorageAttachment::External),
            _ => false,
        };
        // Shrink is a destructive fallback, not a performance preference. If an existing
        // current-session volume has enough exact bytes, payload size crossing an arbitrary
        // threshold must never switch the plan to Shrink.
        let should_probe_shrink = allow_shrink_of_excluded_target
            && (matches!(initial_plan, StagingPlan::Unavailable { .. })
                || initial_plan_uses_external);
        if !allow_shrink_of_excluded_target {
            log::info!(
                "[DISK] 当前安装模式必须保持 {}: 的源卷几何，不将其作为自动暂存缩卷候选",
                exclude_letter
            );
        }

        let shrink_candidate = if should_probe_shrink {
            target.as_ref().and_then(|target| {
                let disk_number = target.disk_number?;
                let partition_number = target.partition_number?;
                let (free_bytes, _total_bytes) = Self::get_volume_space_bytes(exclude_letter)?;
                let (media, attachment) = *profiles
                    .entry(disk_number)
                    .or_insert_with(|| Self::get_storage_profile(disk_number));
                let file_system = Self::get_volume_file_system(exclude_letter);
                let shrink_is_safe = auto_shrink_target_is_safe(
                    file_system.as_deref(),
                    target.bitlocker_status,
                    attachment,
                    target.is_system_partition,
                );
                if !shrink_is_safe {
                    log::warn!(
                        "[DISK] 不自动缩小 {}:：文件系统={:?} BitLocker={} 接口={:?}",
                        exclude_letter,
                        file_system,
                        target.bitlocker_status.as_str(),
                        attachment
                    );
                    return Some(ShrinkCandidate {
                        letter: exclude_letter,
                        disk_number: Some(disk_number),
                        media,
                        attachment,
                        free_bytes,
                        shrink_is_safe: false,
                    });
                }

                log::info!(
                    "[DISK] 缩卷候选 {}: 磁盘={} 分区={} 当前空闲={:.2} GB；真实可缩范围由 VDS Shrink 和 extent 回读决定",
                    exclude_letter,
                    disk_number,
                    partition_number,
                    free_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                Some(ShrinkCandidate {
                    letter: exclude_letter,
                    disk_number: Some(disk_number),
                    media,
                    attachment,
                    free_bytes,
                    shrink_is_safe: true,
                })
            })
        } else {
            None
        };

        match select_staging_plan(
            payload_size_bytes,
            target_disk_number,
            selection_candidates,
            shrink_candidate,
        ) {
            StagingPlan::Existing {
                letter,
                required_bytes,
            } => {
                log::info!(
                    "[DISK] 选择现有数据分区 {}:，全部文件加 2 GiB 共需 {:.2} GB",
                    letter,
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                Ok(Some((format!("{}:", letter), None)))
            }
            StagingPlan::ShrinkTarget {
                letter,
                size_mb,
                required_bytes,
            } => {
                let target = target.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("无法取得目标分区 {}: 的稳定身份", exclude_letter)
                })?;
                let disk_number = target
                    .disk_number
                    .ok_or_else(|| anyhow::anyhow!("无法取得目标分区物理磁盘号"))?;
                let partition_number = target
                    .partition_number
                    .ok_or_else(|| anyhow::anyhow!("无法取得目标分区号"))?;
                log::info!(
                    "[DISK] 将从 {}: 缩出 {} MB 临时分区，全部文件加 2 GiB 共需 {:.2} GB",
                    letter,
                    size_mb,
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                let transaction =
                    Self::shrink_and_create_partition_with_marker(ShrinkAndCreateMarkerRequest {
                        source_letter: letter,
                        desired_size_mb: size_mb,
                        payload_size_bytes,
                        expected_source_identity: lr_core::windows_storage::VolumeIdentity {
                            disk_number,
                            offset_bytes: target
                                .partition_offset_bytes
                                .context("无法取得目标分区起始偏移")?,
                            extent_length_bytes: target
                                .partition_size_bytes
                                .context("无法取得目标分区精确长度")?,
                        },
                        expected_disk_number: disk_number,
                        expected_partition_number: partition_number,
                        expected_bitlocker_status: target.bitlocker_status,
                        is_current_system: target.is_system_partition,
                    })?;
                let target_partition = transaction.target_partition();
                Ok(Some((target_partition, Some(transaction))))
            }
            StagingPlan::Unavailable { required_bytes } => {
                log::error!(
                    "[DISK] 没有能容纳全部暂存文件加 2 GiB 的位置，需要 {:.2} GB",
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                );
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        auto_shrink_target_is_safe, classify_staging_drive_type, logical_sector_capacity_ceiling,
        observed_shrink_bytes, preferred_install_partition_index, reclaimed_tail,
        validate_created_in_reclaimed_tail, validate_formatted_payload_capacity,
        vds_aligned_reclaim_bytes, vds_alignment_value_for_disk_size, Partition, PartitionStyle,
        StagingDriveKind, DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };
    use crate::core::bitlocker::VolumeStatus;
    use lr_core::data_staging::{StorageAttachment, StorageMedia};
    use lr_core::windows_storage::PartitionKind;

    fn install_candidate(
        letter: &str,
        disk_number: u32,
        partition_number: u32,
        offset_bytes: u64,
        kind: Option<PartitionKind>,
        media: StorageMedia,
    ) -> Partition {
        Partition {
            letter: letter.to_owned(),
            total_size_mb: 64 * 1024,
            free_size_mb: 32 * 1024,
            free_size_bytes: 32 * 1024 * 1024 * 1024,
            label: String::new(),
            is_system_partition: false,
            has_windows: false,
            partition_style: PartitionStyle::GPT,
            disk_number: Some(disk_number),
            partition_number: Some(partition_number),
            disk_size_bytes: Some(128 * 1024 * 1024 * 1024),
            partition_offset_bytes: Some(offset_bytes),
            partition_size_bytes: Some(64 * 1024 * 1024 * 1024),
            partition_kind: kind,
            install_target_eligible: kind == Some(PartitionKind::BasicData),
            storage_media: media,
            stable_identity: None,
            bitlocker_status: VolumeStatus::NotEncrypted,
        }
    }

    #[test]
    fn pe_default_prefers_first_ssd_disk_then_lowest_partition_offset() {
        let partitions = vec![
            install_candidate(
                "C:",
                0,
                1,
                1024 * 1024,
                Some(PartitionKind::BasicData),
                StorageMedia::Rotational,
            ),
            install_candidate(
                "X:",
                1,
                1,
                1024 * 1024,
                Some(PartitionKind::BasicData),
                StorageMedia::SolidState,
            ),
            install_candidate(
                "S:",
                1,
                2,
                2 * 1024 * 1024,
                Some(PartitionKind::EfiSystem),
                StorageMedia::SolidState,
            ),
            install_candidate(
                "M:",
                1,
                3,
                3 * 1024 * 1024,
                Some(PartitionKind::MicrosoftReserved),
                StorageMedia::SolidState,
            ),
            install_candidate(
                "G:",
                1,
                5,
                5 * 1024 * 1024,
                Some(PartitionKind::BasicData),
                StorageMedia::SolidState,
            ),
            install_candidate(
                "D:",
                1,
                4,
                4 * 1024 * 1024 + 512,
                Some(PartitionKind::Recovery),
                StorageMedia::SolidState,
            ),
            install_candidate(
                "E:",
                2,
                1,
                1024 * 1024,
                Some(PartitionKind::BasicData),
                StorageMedia::SolidState,
            ),
        ];

        assert_eq!(
            preferred_install_partition_index(&partitions, true),
            Some(4),
            "disk 1 is the first SSD with an ordinary user-data partition; recovery/service roles are never defaults"
        );
    }

    #[test]
    fn pe_default_falls_back_to_first_disk_when_no_ssd_is_confirmed() {
        let partitions = vec![
            install_candidate(
                "F:",
                3,
                1,
                1024 * 1024,
                Some(PartitionKind::BasicData),
                StorageMedia::Rotational,
            ),
            install_candidate(
                "D:",
                0,
                4,
                8 * 1024 * 1024 + 512,
                Some(PartitionKind::BasicData),
                StorageMedia::Unknown,
            ),
            install_candidate(
                "C:",
                0,
                2,
                4 * 1024 * 1024 + 512,
                Some(PartitionKind::BasicData),
                StorageMedia::Rotational,
            ),
        ];
        assert_eq!(
            preferred_install_partition_index(&partitions, true),
            Some(2)
        );
    }

    #[test]
    fn pe_default_does_not_guess_unresolved_or_malformed_partition_inventory() {
        let mut unresolved =
            install_candidate("C:", 0, 1, 1024 * 1024, None, StorageMedia::SolidState);
        unresolved.partition_offset_bytes = None;
        let malformed = install_candidate(
            "not-a-drive",
            0,
            2,
            2 * 1024 * 1024,
            Some(PartitionKind::BasicData),
            StorageMedia::SolidState,
        );
        assert_eq!(
            preferred_install_partition_index(&[unresolved, malformed], true),
            None
        );
    }

    #[test]
    fn desktop_default_remains_the_current_system_partition() {
        let first = install_candidate(
            "D:",
            0,
            1,
            1024 * 1024,
            Some(PartitionKind::BasicData),
            StorageMedia::SolidState,
        );
        let mut current = install_candidate(
            "C:",
            1,
            1,
            1024 * 1024,
            Some(PartitionKind::BasicData),
            StorageMedia::Rotational,
        );
        current.is_system_partition = true;
        assert_eq!(
            preferred_install_partition_index(&[first, current], false),
            Some(1)
        );
    }

    #[cfg(windows)]
    #[test]
    fn storage_descriptors_require_complete_documented_headers() {
        use super::{
            validated_seek_penalty_descriptor, validated_storage_bus_type,
            validated_storage_device_number,
        };
        use windows::Win32::System::Ioctl::{
            DEVICE_SEEK_PENALTY_DESCRIPTOR, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
        };

        let number_size = std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32;
        assert_eq!(
            validated_storage_device_number(number_size, 0, 1),
            Some((0, 1))
        );
        assert_eq!(validated_storage_device_number(number_size - 1, 0, 1), None);
        assert_eq!(
            validated_storage_device_number(number_size, u32::MAX, 1),
            None
        );
        assert_eq!(
            validated_storage_device_number(number_size, 0, u32::MAX),
            None
        );

        let seek_size = std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32;
        assert_eq!(
            validated_seek_penalty_descriptor(seek_size, seek_size, seek_size, false),
            Some(false)
        );
        assert_eq!(
            validated_seek_penalty_descriptor(seek_size - 1, seek_size, seek_size, false),
            None
        );
        assert_eq!(
            validated_seek_penalty_descriptor(seek_size, seek_size - 1, seek_size, false),
            None
        );
        assert_eq!(
            validated_seek_penalty_descriptor(seek_size, seek_size, seek_size - 1, false),
            None
        );

        let device_size = std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32;
        assert_eq!(
            validated_storage_bus_type(device_size, device_size, device_size, 17),
            Some(17)
        );
        assert_eq!(
            validated_storage_bus_type(device_size, device_size, device_size - 1, 17),
            None
        );
    }

    #[test]
    fn staging_drive_types_exclude_optical_remote_and_ram_disks() {
        assert_eq!(
            classify_staging_drive_type(DRIVE_FIXED),
            Some(StagingDriveKind::Fixed)
        );
        assert_eq!(
            classify_staging_drive_type(DRIVE_REMOVABLE),
            Some(StagingDriveKind::Removable)
        );
        assert_eq!(classify_staging_drive_type(DRIVE_CDROM), None);
        assert_eq!(classify_staging_drive_type(DRIVE_REMOTE), None);
        assert_eq!(classify_staging_drive_type(DRIVE_RAMDISK), None);
    }

    #[test]
    fn post_error_recovery_only_accepts_an_observed_tail_shrink_of_the_same_extent() {
        let before = lr_core::windows_storage::VolumeIdentity {
            disk_number: 4,
            offset_bytes: 4096 + 512,
            extent_length_bytes: 80 * 1024 * 1024 + 4096,
        };
        assert_eq!(observed_shrink_bytes(before, before).unwrap(), None);
        assert_eq!(
            observed_shrink_bytes(
                before,
                lr_core::windows_storage::VolumeIdentity {
                    extent_length_bytes: before.extent_length_bytes - (8 * 1024 * 1024 + 512),
                    ..before
                }
            )
            .unwrap(),
            Some(8 * 1024 * 1024 + 512),
            "a sector-valid non-MiB provider result remains recoverable"
        );
        assert!(observed_shrink_bytes(
            before,
            lr_core::windows_storage::VolumeIdentity {
                disk_number: 5,
                ..before
            }
        )
        .is_err());
        assert!(observed_shrink_bytes(
            before,
            lr_core::windows_storage::VolumeIdentity {
                extent_length_bytes: before.extent_length_bytes + 512,
                ..before
            }
        )
        .is_err());
    }

    #[test]
    fn vds_alignment_tier_uses_documented_size_specific_registry_names() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            vds_alignment_value_for_disk_size(4 * GIB - 1),
            ("LessThan4GB", 64 * 1024)
        );
        assert_eq!(
            vds_alignment_value_for_disk_size(4 * GIB),
            ("Between4_8GB", 1024 * 1024)
        );
        assert_eq!(
            vds_alignment_value_for_disk_size(8 * GIB),
            ("Between8_32GB", 1024 * 1024)
        );
        assert_eq!(
            vds_alignment_value_for_disk_size(32 * GIB),
            ("Between8_32GB", 1024 * 1024)
        );
        assert_eq!(
            vds_alignment_value_for_disk_size(32 * GIB + 1),
            ("GreaterThan32GB", 1024 * 1024)
        );
    }

    #[test]
    fn vds_shrink_budget_handles_non_mib_geometry_and_admin_alignment_override() {
        let source = lr_core::windows_storage::VolumeIdentity {
            disk_number: 0,
            offset_bytes: 331_350_016,
            extent_length_bytes: 85_899_329_024 - 331_350_016,
        };
        let partition = 9_606 * 1024 * 1024;
        let shrink = vds_aligned_reclaim_bytes(source, partition, 1024 * 1024).unwrap();
        let source_end = source.offset_bytes + source.extent_length_bytes;
        let reclaimed_start = source_end - shrink;
        assert_eq!(reclaimed_start % (1024 * 1024), 0);
        assert!(shrink >= partition);
        assert!(shrink - partition < 1024 * 1024);

        // An administrator-defined, non-MiB value is consumed as bytes without substituting a
        // hard-coded geometry rule.  The resulting provider start is passed through unchanged.
        let override_alignment = 3 * 64 * 1024;
        let overridden = vds_aligned_reclaim_bytes(source, partition, override_alignment).unwrap();
        assert_eq!((source_end - overridden) % override_alignment, 0);
        assert!(overridden >= partition);
        assert!(overridden - partition < override_alignment);
    }

    #[test]
    fn vds_shrink_budget_rejects_only_unusable_zero_alignment_and_overflow() {
        let source = lr_core::windows_storage::VolumeIdentity {
            disk_number: 7,
            offset_bytes: 512,
            extent_length_bytes: 8 * 1024 * 1024 + 4096,
        };
        assert!(vds_aligned_reclaim_bytes(source, 1024 * 1024, 0).is_err());
        assert!(vds_aligned_reclaim_bytes(source, u64::MAX, 4096).is_err());
    }

    #[test]
    fn dual_boot_shrink_capacity_uses_the_current_logical_sector_without_fixed_geometry() {
        let observed_failure_minimum = 35_829_648_602_u64;
        assert_eq!(
            logical_sector_capacity_ceiling(observed_failure_minimum, 512).unwrap(),
            35_829_648_896
        );
        assert_eq!(
            logical_sector_capacity_ceiling(observed_failure_minimum, 4096).unwrap(),
            35_829_649_408
        );
        assert_eq!(logical_sector_capacity_ceiling(8192, 4096).unwrap(), 8192);
        assert!(logical_sector_capacity_ceiling(1, 0).is_err());
        assert!(logical_sector_capacity_ceiling(u64::MAX, 4096).is_err());
    }

    #[test]
    fn reclaimed_tail_is_the_only_authorization_before_the_provider_create_boundary() {
        let before = lr_core::windows_storage::VolumeIdentity {
            disk_number: 3,
            offset_bytes: 4096 + 512,
            extent_length_bytes: 100 * 1024 * 1024 + 4096,
        };
        let after = lr_core::windows_storage::VolumeIdentity {
            extent_length_bytes: before.extent_length_bytes - (12 * 1024 * 1024 + 512),
            ..before
        };
        let tail_start = after.offset_bytes + after.extent_length_bytes;
        let tail_end = before.offset_bytes + before.extent_length_bytes;
        let tail = reclaimed_tail(before, after).unwrap();
        assert_eq!(tail.offset_bytes, tail_start);
        assert_eq!(tail.offset_bytes + tail.length_bytes, tail_end);
        assert_eq!(tail.length_bytes, 12 * 1024 * 1024 + 512);
        assert_ne!(tail.length_bytes % (1024 * 1024), 0);
    }

    #[test]
    fn staging_extent_and_formatted_free_space_both_guard_the_payload() {
        let before = lr_core::windows_storage::VolumeIdentity {
            disk_number: 7,
            offset_bytes: 1024 * 1024 + 512,
            extent_length_bytes: 80 * 1024 * 1024 + 4096,
        };
        let after = lr_core::windows_storage::VolumeIdentity {
            extent_length_bytes: before.extent_length_bytes - (9 * 1024 * 1024 + 512),
            ..before
        };
        let created = lr_core::windows_storage::CreatedPartition {
            offset_bytes: after.offset_bytes + after.extent_length_bytes + 512,
            size_bytes: 8 * 1024 * 1024,
        };
        validate_created_in_reclaimed_tail(before, after, created).unwrap();
        validate_formatted_payload_capacity(7 * 1024 * 1024, created.size_bytes, 7 * 1024 * 1024)
            .unwrap();
        assert!(validate_formatted_payload_capacity(
            7 * 1024 * 1024 - 1,
            created.size_bytes,
            7 * 1024 * 1024,
        )
        .is_err());

        let outside = lr_core::windows_storage::CreatedPartition {
            offset_bytes: before.offset_bytes + before.extent_length_bytes - 1024,
            size_bytes: 4096,
        };
        assert!(validate_created_in_reclaimed_tail(before, after, outside).is_err());
    }

    #[test]
    fn stable_unlocked_bitlocker_current_system_volume_can_be_shrunk() {
        assert!(auto_shrink_target_is_safe(
            Some("NTFS"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::Internal,
            true,
        ));
        assert!(auto_shrink_target_is_safe(
            Some("ntfs"),
            VolumeStatus::NotEncrypted,
            StorageAttachment::Internal,
            false,
        ));
    }

    #[test]
    fn unsafe_bitlocker_and_storage_states_remain_fail_closed() {
        for status in [
            VolumeStatus::EncryptedLocked,
            VolumeStatus::Encrypting,
            VolumeStatus::Decrypting,
            VolumeStatus::Unknown,
        ] {
            assert!(!auto_shrink_target_is_safe(
                Some("NTFS"),
                status,
                StorageAttachment::Internal,
                true,
            ));
        }
        assert!(!auto_shrink_target_is_safe(
            Some("NTFS"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::Internal,
            false,
        ));
        assert!(!auto_shrink_target_is_safe(
            Some("NTFS"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::External,
            true,
        ));
        assert!(!auto_shrink_target_is_safe(
            Some("FAT32"),
            VolumeStatus::EncryptedUnlocked,
            StorageAttachment::Internal,
            true,
        ));
    }
}
