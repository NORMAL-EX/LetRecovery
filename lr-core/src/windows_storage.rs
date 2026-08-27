//! Shared, documented Win32 storage-management boundary.
//!
//! Configuration operations use Virtual Disk Service (VDS) COM interfaces and
//! documented disk IOCTLs. Volume shrink keeps VDS as the primary provider and, on
//! Windows 8 or later, may use the documented Storage Management provider only when
//! VDS fails before returning an asynchronous operation. Callers remain responsible for presenting a
//! destructive-operation confirmation and for comparing a fresh disk/partition
//! fingerprint immediately before calling this module. Every operation returns
//! the original HRESULT context and callers must re-enumerate afterward.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskStyle {
    Mbr,
    Gpt,
}

/// Physical storage bus classification used for fail-closed install defaults.
///
/// Only an explicit `BusTypeNvme` result is classified as NVMe. RAID/VMD,
/// virtual and failed queries are deliberately not guessed to be NVMe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskBusType {
    Nvme,
    Other,
}

/// Sector geometry reported by the current physical-disk device stack.
///
/// These values come from `IOCTL_STORAGE_QUERY_PROPERTY` with
/// `StorageAccessAlignmentProperty`. They are deliberately not synthesized from fixed 512-byte or
/// 4-KiB defaults: callers that require raw-I/O alignment must stop when the query is unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskSectorGeometry {
    pub logical_sector_bytes: u32,
    pub physical_sector_bytes: u32,
    /// Offset of logical sector zero within the first physical sector, in bytes.
    pub sector_alignment_offset_bytes: u32,
}

fn validated_disk_sector_geometry(
    descriptor_version: u32,
    descriptor_size: u32,
    returned_bytes: u32,
    known_descriptor_size: u32,
    logical_sector_bytes: u32,
    physical_sector_bytes: u32,
    sector_alignment_offset_bytes: u32,
) -> Result<DiskSectorGeometry, StorageError> {
    if returned_bytes < known_descriptor_size
        || descriptor_version < known_descriptor_size
        || descriptor_size < known_descriptor_size
    {
        return Err(StorageError::new(
            "query physical disk sector geometry",
            format!(
                "storage alignment descriptor is truncated: returned={returned_bytes} version={descriptor_version} size={descriptor_size} required={known_descriptor_size}"
            ),
        ));
    }
    if logical_sector_bytes == 0 || physical_sector_bytes == 0 {
        return Err(StorageError::new(
            "query physical disk sector geometry",
            "storage alignment descriptor reported a zero logical or physical sector size",
        ));
    }
    // A physical sector consists of complete logical sectors. Rejecting a contradictory provider
    // descriptor is safer than guessing which field should govern destructive raw I/O.
    if physical_sector_bytes < logical_sector_bytes
        || !physical_sector_bytes.is_multiple_of(logical_sector_bytes)
    {
        return Err(StorageError::new(
            "query physical disk sector geometry",
            format!(
                "storage alignment descriptor reported incompatible sector sizes: logical={logical_sector_bytes} physical={physical_sector_bytes}"
            ),
        ));
    }
    // Microsoft defines this as a byte offset within the first physical sector. When present, the
    // offset is expressed in complete logical sectors (for example, three logical sectors).
    if sector_alignment_offset_bytes >= physical_sector_bytes
        || !sector_alignment_offset_bytes.is_multiple_of(logical_sector_bytes)
    {
        return Err(StorageError::new(
            "query physical disk sector geometry",
            format!(
                "storage alignment descriptor reported an invalid sector-alignment offset: offset={sector_alignment_offset_bytes} logical={logical_sector_bytes} physical={physical_sector_bytes}"
            ),
        ));
    }
    Ok(DiskSectorGeometry {
        logical_sector_bytes,
        physical_sector_bytes,
        sector_alignment_offset_bytes,
    })
}

/// A currently present disk interface returned by SetupAPI.
///
/// `disk_number` is only a current-session locator. `device_path` is the opaque path returned by
/// `SetupDiGetDeviceInterfaceDetailW`; callers may pass it to `CreateFileW`, but must never parse
/// it or replace it with a guessed `PhysicalDriveN` alias for inventory IOCTLs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentDiskInterface {
    pub disk_number: u32,
    pub device_path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSystem {
    Ntfs,
    Fat,
    Fat32,
    ExFat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriveKind {
    Removable,
    Fixed,
    Remote,
    Optical,
    RamDisk,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatOptions {
    pub file_system: FileSystem,
    pub label: String,
    /// Zero lets Windows choose the default allocation-unit size.
    pub allocation_unit_size: u32,
    pub quick: bool,
    /// Requests VDS to dismount an in-use volume before formatting it.
    ///
    /// Callers may enable this only after revalidating the destructive target and proving that
    /// the executable, image and other required inputs are not stored on that volume.
    pub force_dismount: bool,
}

/// Extracts an ASCII drive letter from ordinary or verbatim Windows paths.
///
/// Relative and UNC paths deliberately return `None`; callers must resolve relative paths before
/// using this helper as a destructive-operation guard.
pub fn path_drive_letter(path: &std::path::Path) -> Option<char> {
    let text = path.as_os_str().to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix(r"\\.\"))
        .unwrap_or(&text);
    let bytes = text.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| (bytes[0] as char).to_ascii_uppercase())
}

impl FileSystem {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ntfs => "NTFS",
            Self::Fat => "FAT",
            Self::Fat32 => "FAT32",
            Self::ExFat => "EXFAT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionKind {
    BasicData,
    EfiSystem,
    MicrosoftReserved,
    Recovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GptPartitionMetadata {
    /// In-memory GUID bytes as returned by `PARTITION_INFORMATION_GPT`.
    pub partition_id: [u8; 16],
    pub attributes: u64,
    pub name: [u16; 36],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatePartitionRequest {
    pub disk_number: u32,
    /// Zero selects the first usable free extent. An explicit value is passed to VDS unchanged
    /// and must be contained by one current provider free extent.
    pub offset_bytes: u64,
    /// Requested capacity for the provider-created partition. This must be non-zero.
    /// A caller that wants all remaining space must first read the current provider extent and
    /// pass that concrete length; zero is not overloaded as a second creation mode.
    pub size_bytes: u64,
    pub kind: PartitionKind,
    pub file_system: Option<FileSystem>,
    pub label: String,
    pub drive_letter: Option<char>,
    pub active: bool,
    /// Used only when recreating an existing GPT partition after an offline block move.
    /// Ordinary partition creation must leave this as `None`.
    pub preserve_gpt_metadata: Option<GptPartitionMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreatedPartition {
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreeExtent {
    pub offset_bytes: u64,
    pub length_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VolumeIdentity {
    pub disk_number: u32,
    pub offset_bytes: u64,
    pub extent_length_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StableDiskIdentity {
    /// Uninitialized disk identity; valid in a whole-disk snapshot only when a device ID exists.
    Raw,
    Gpt {
        disk_id: [u8; 16],
    },
    Mbr {
        signature: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StablePartitionIdentity {
    Gpt { partition_id: [u8; 16] },
    Mbr { partition_number: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableVolumeIdentity {
    pub extent: VolumeIdentity,
    pub disk: StableDiskIdentity,
    pub partition: StablePartitionIdentity,
    /// SHA-256 of the normalized identifiers returned by `StorageDeviceIdProperty`.
    ///
    /// Some legacy/virtual storage stacks expose no documented device identifier. `None` keeps
    /// that limitation explicit; callers must not describe such a probe as replacement-proof.
    pub device_id_hash: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiskLayoutPartitionToken {
    Gpt {
        partition_type: [u8; 16],
        partition_id: [u8; 16],
        attributes: u64,
    },
    Mbr {
        partition_type: u8,
        boot_indicator: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DiskLayoutPartitionSnapshot {
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub token: DiskLayoutPartitionToken,
}

// `PARTITION_INFORMATION_GPT::Attributes` values documented by Microsoft. These are inventory
// semantics, not geometry heuristics: platform/service, read-only, shadow-copy, hidden, and
// no-drive-letter partitions are not ordinary Windows installation targets even if WinPE has
// temporarily assigned them a letter.
const GPT_ATTRIBUTE_PLATFORM_REQUIRED: u64 = 0x0000_0000_0000_0001;
const GPT_BASIC_DATA_ATTRIBUTE_READ_ONLY: u64 = 0x1000_0000_0000_0000;
const GPT_BASIC_DATA_ATTRIBUTE_SHADOW_COPY: u64 = 0x2000_0000_0000_0000;
const GPT_BASIC_DATA_ATTRIBUTE_HIDDEN: u64 = 0x4000_0000_0000_0000;
const GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER: u64 = 0x8000_0000_0000_0000;
const GPT_BASIC_DATA_PARTITION_TYPE: [u8; 16] =
    0xebd0_a0a2_b9e5_4433_87c0_68b6_b726_99c7_u128.to_le_bytes();

/// Return whether a canonical layout token represents an ordinary user-data partition that may
/// be offered as a Windows installation target.
///
/// This deliberately consumes the OS-reported partition type and attributes only. It does not
/// impose alignment, capacity, label, free-space, disk-number, or layout-shape heuristics.
pub fn partition_token_is_installable_user_data(token: DiskLayoutPartitionToken) -> bool {
    match token {
        DiskLayoutPartitionToken::Gpt {
            partition_type,
            attributes,
            ..
        } => {
            const EXCLUDED_ATTRIBUTES: u64 = GPT_ATTRIBUTE_PLATFORM_REQUIRED
                | GPT_BASIC_DATA_ATTRIBUTE_READ_ONLY
                | GPT_BASIC_DATA_ATTRIBUTE_SHADOW_COPY
                | GPT_BASIC_DATA_ATTRIBUTE_HIDDEN
                | GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER;
            partition_type == GPT_BASIC_DATA_PARTITION_TYPE && attributes & EXCLUDED_ATTRIBUTES == 0
        }
        // Whitelist ordinary DOS/Windows FAT, FAT32, IFS/NTFS/exFAT and FAT16-LBA types. OEM
        // (including 0x27 recovery), EFI, extended-container, dynamic and protective entries are
        // intentionally absent.
        DiskLayoutPartitionToken::Mbr { partition_type, .. } => {
            matches!(
                partition_type,
                0x01 | 0x04 | 0x06 | 0x07 | 0x0b | 0x0c | 0x0e
            )
        }
    }
}

/// Canonical physical-disk layout used for normal-system to WinPE handoff checks.
///
/// It is built only from documented physical-disk IOCTLs. Partition numbers, drive letters,
/// labels, file systems and provider enumeration order are deliberately excluded because they
/// can differ across boots and between VDS providers while naming the same on-disk layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskLayoutSnapshot {
    pub disk_size_bytes: u64,
    pub disk: StableDiskIdentity,
    pub device_id_hash: Option<[u8; 32]>,
    pub partitions: Vec<DiskLayoutPartitionSnapshot>,
}

/// Reconcile observations made through every currently present SetupAPI disk-interface path that
/// resolves to one current disk number.
///
/// A disk number is only a session locator. Vendor filter stacks may expose more than one opaque
/// interface path for the same disk, so one matching path is not evidence that the others agree.
/// Capacity, partition-table identity and extents must agree. A device-level identifier may be
/// unavailable on one alias, but two identifiers that are both present and disagree are a real
/// conflict. Such conflicts are reported as `UntrustedStorage` and must stop destructive work.
fn reconcile_present_disk_snapshots(
    disk_number: u32,
    mut observations: Vec<(String, DiskLayoutSnapshot)>,
) -> Result<(String, DiskLayoutSnapshot), StorageError> {
    observations.sort_by(|left, right| left.0.cmp(&right.0));
    let Some((selected_path, mut trusted)) = observations.first().cloned() else {
        return Err(StorageError::new(
            "bind present physical disk",
            format!(
                "UntrustedStorage: SetupAPI exposed no usable present disk interface for current disk {disk_number}"
            ),
        ));
    };
    for (path, observed) in observations.into_iter().skip(1) {
        let fixed_conflict = trusted.disk_size_bytes != observed.disk_size_bytes
            || trusted.disk != observed.disk
            || trusted.partitions != observed.partitions;
        let device_id_conflict = matches!(
            (trusted.device_id_hash, observed.device_id_hash),
            (Some(left), Some(right)) if left != right
        );
        if fixed_conflict || device_id_conflict {
            return Err(StorageError::new(
                "bind present physical disk",
                format!(
                    "UntrustedStorage: present SetupAPI interfaces for current disk {disk_number} disagree; selected={selected_path:?}, conflicting={path:?}"
                ),
            ));
        }
        if trusted.device_id_hash.is_none() {
            trusted.device_id_hash = observed.device_id_hash;
        }
    }
    Ok((selected_path, trusted))
}

/// Reconcile `STORAGE_DEVICE_DESCRIPTOR.BusType` values returned through every present opaque
/// interface path that already resolved to the same current physical disk.
///
/// Bus type is only auxiliary install evidence; it is never a disk identity. Even so, selecting
/// one disagreeing filter alias would make driver defaults depend on SetupAPI enumeration order.
/// Two successful, contradictory descriptors therefore make the current storage observation
/// untrusted instead of being guessed or silently preferred.
fn reconcile_present_disk_bus_types(
    disk_number: u32,
    mut observations: Vec<(String, DiskBusType)>,
) -> Result<DiskBusType, StorageError> {
    observations.sort_by(|left, right| left.0.cmp(&right.0));
    let Some((selected_path, trusted)) = observations.first().cloned() else {
        return Err(StorageError::new(
            "query physical disk bus",
            format!(
                "UntrustedStorage: no current present disk interface returned a usable bus type for disk {disk_number}"
            ),
        ));
    };
    for (path, observed) in observations.into_iter().skip(1) {
        if observed != trusted {
            return Err(StorageError::new(
                "query physical disk bus",
                format!(
                    "UntrustedStorage: present SetupAPI interfaces for current disk {disk_number} report conflicting bus types; selected={selected_path:?}, conflicting={path:?}"
                ),
            ));
        }
    }
    Ok(trusted)
}

/// Close the current-session chain from a mounted volume, through its volume-GUID path and exact
/// extent, to one partition in the canonical physical-disk layout. The GUID itself is a live
/// object locator, not a cross-reboot fingerprint; agreement of all three observations is the
/// write-boundary fact.
fn verify_current_volume_identity_closure(
    mounted_extent: VolumeIdentity,
    volume_guid_extent: VolumeIdentity,
    disk_snapshot: &DiskLayoutSnapshot,
) -> Result<DiskLayoutPartitionSnapshot, StorageError> {
    if !same_volume_identity(mounted_extent, volume_guid_extent) {
        return Err(StorageError::new(
            "bind current volume identity",
            "UntrustedStorage: drive-letter and volume-GUID handles report different physical extents",
        ));
    }
    let matches = disk_snapshot
        .partitions
        .iter()
        .copied()
        .filter(|partition| {
            partition.offset_bytes == mounted_extent.offset_bytes
                && partition.size_bytes == mounted_extent.extent_length_bytes
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(StorageError::new(
            "bind current volume identity",
            format!(
                "UntrustedStorage: canonical physical-disk layout contains {} exact partition records for the mounted volume extent",
                matches.len()
            ),
        ));
    }
    Ok(matches[0])
}

fn resolve_device_number_with_extended_fallback<T>(
    legacy: Result<T, StorageError>,
    extended: impl FnOnce() -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    match legacy {
        Ok(value) => Ok(value),
        Err(legacy_error) => extended().map_err(|extended_error| {
            StorageError::new(
                "resolve opened disk device number",
                format!(
                    "legacy device-number query failed ({legacy_error}); extended device-number query failed ({extended_error})"
                ),
            )
        }),
    }
}

fn storage_management_partition_matches_current_extent(
    drive_letter: char,
    disk_number: u32,
    partition_number: u32,
    size_bytes: u64,
    expected_drive_letter: char,
    expected_extent: VolumeIdentity,
    canonical_partition_number: u32,
) -> bool {
    drive_letter.eq_ignore_ascii_case(&expected_drive_letter)
        && disk_number == expected_extent.disk_number
        && partition_number == canonical_partition_number
        && size_bytes == expected_extent.extent_length_bytes
}

pub fn same_physical_partition(left: VolumeIdentity, right: VolumeIdentity) -> bool {
    left.disk_number == right.disk_number && left.offset_bytes == right.offset_bytes
}

/// Compares only the immutable physical range used to authorize later volume writes.
///
/// Free space, file system, label and other mutable volume snapshot fields deliberately do not
/// participate: formatting and image application are expected to change them without changing
/// which physical partition the drive letter names.
pub fn same_volume_identity(left: VolumeIdentity, right: VolumeIdentity) -> bool {
    same_physical_partition(left, right) && left.extent_length_bytes == right.extent_length_bytes
}

pub fn same_stable_volume_identity(
    left: StableVolumeIdentity,
    right: StableVolumeIdentity,
) -> bool {
    same_volume_identity(left.extent, right.extent)
        && left.disk == right.disk
        && same_optional_device_id(left.device_id_hash, right.device_id_hash)
        && same_stable_partition_token(left.partition, right.partition)
}

pub fn same_stable_partition_identity(
    left: StableVolumeIdentity,
    right: StableVolumeIdentity,
) -> bool {
    same_physical_partition(left.extent, right.extent)
        && left.disk == right.disk
        && same_optional_device_id(left.device_id_hash, right.device_id_hash)
        && same_stable_partition_token(left.partition, right.partition)
}

fn same_optional_device_id(left: Option<[u8; 32]>, right: Option<[u8; 32]>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    }
}

/// Verify the authoritative volume extent read back after a VDS shrink.
///
/// `QueryMaxReclaimableBytes` is only an estimate and the async output is provider reporting.
/// Windows may also round the requested byte count to a file-system cluster boundary.  The
/// current `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` result is therefore the completion fact: the
/// same volume must still start on the same current disk and the actual reclaimed range must be
/// non-empty and at least the caller's minimum.  No MiB or exact-request equality is imposed.
fn verified_shrink_reclaimed_bytes(
    expected: VolumeIdentity,
    actual: VolumeIdentity,
    minimum_bytes: u64,
) -> Result<u64, StorageError> {
    if actual.disk_number != expected.disk_number || actual.offset_bytes != expected.offset_bytes {
        return Err(StorageError::new(
            "verify shrunk volume",
            "post-operation volume no longer starts at the authorized current extent",
        ));
    }
    let reclaimed = expected
        .extent_length_bytes
        .checked_sub(actual.extent_length_bytes)
        .ok_or_else(|| {
            StorageError::new(
                "verify shrunk volume",
                "post-operation volume grew instead of shrinking",
            )
        })?;
    if reclaimed == 0 || reclaimed < minimum_bytes {
        return Err(StorageError::new(
            "verify shrunk volume",
            format!(
                "actual reclaimed range is {reclaimed} bytes, below required minimum {minimum_bytes} bytes"
            ),
        ));
    }
    Ok(reclaimed)
}

/// Select the exact partition size passed to `MSFT_Partition.Resize`.
///
/// Microsoft defines `SizeMin`/`SizeMax` and `Resize.Size` in bytes. VDS accepts a desired and a
/// minimum reclaim amount, while the Storage Management API accepts one final size. Preserve the
/// same contract by preferring the desired final size, clamping only to the provider's current
/// `SizeMin`, and refusing the fallback when that would reclaim less than the caller's minimum.
fn storage_management_shrink_target(
    current_size: u64,
    desired_reclaim: u64,
    minimum_reclaim: u64,
    size_min: u64,
    size_max: u64,
) -> Result<u64, StorageError> {
    if desired_reclaim == 0
        || minimum_reclaim == 0
        || minimum_reclaim > desired_reclaim
        || desired_reclaim >= current_size
        || size_min == 0
        || size_min > size_max
        || current_size < size_min
        || current_size > size_max
    {
        return Err(StorageError::new(
            "select Storage Management shrink size",
            "invalid caller or MSFT_Partition supported-size range",
        ));
    }
    let desired_size = current_size - desired_reclaim;
    let largest_allowed_size = current_size - minimum_reclaim;
    let target_size = desired_size.max(size_min);
    if target_size > largest_allowed_size || target_size >= current_size {
        return Err(StorageError::new(
            "select Storage Management shrink size",
            format!(
                "MSFT_Partition can shrink only to {size_min} bytes, which cannot reclaim the required minimum {minimum_reclaim} bytes from {current_size} bytes"
            ),
        ));
    }
    Ok(target_size)
}

fn verified_extend_added_bytes(
    expected: VolumeIdentity,
    actual: VolumeIdentity,
    minimum_bytes: u64,
    provider_start: u64,
    provider_end: u64,
) -> Result<u64, StorageError> {
    if actual.disk_number != expected.disk_number || actual.offset_bytes != expected.offset_bytes {
        return Err(StorageError::new(
            "verify extended volume",
            "post-operation volume no longer starts at the authorized current extent",
        ));
    }
    let expected_end = expected
        .offset_bytes
        .checked_add(expected.extent_length_bytes)
        .ok_or_else(|| StorageError::new("verify extended volume", "volume end overflow"))?;
    let actual_end = actual
        .offset_bytes
        .checked_add(actual.extent_length_bytes)
        .ok_or_else(|| StorageError::new("verify extended volume", "actual volume end overflow"))?;
    let added = actual
        .extent_length_bytes
        .checked_sub(expected.extent_length_bytes)
        .ok_or_else(|| {
            StorageError::new("verify extended volume", "volume shrank during extend")
        })?;
    if added < minimum_bytes
        || provider_start > expected_end
        || expected_end >= provider_end
        || actual_end > provider_end
    {
        return Err(StorageError::new(
            "verify extended volume",
            "actual extension is below the requested minimum or outside the authorized adjacent provider extent",
        ));
    }
    Ok(added)
}

/// Return the canonical byte boundary available immediately after a basic volume.
///
/// The snapshot comes from `IOCTL_DISK_GET_DRIVE_LAYOUT_EX`, not VDS cache inventory. The real
/// `IVdsVolume::Extend` call still decides whether the file system and provider can perform the
/// operation; this helper only proves that its requested growth cannot cross another partition or
/// the current disk capacity.
fn canonical_adjacent_authorized_end(
    snapshot: &DiskLayoutSnapshot,
    expected: VolumeIdentity,
) -> Result<u64, StorageError> {
    let expected_end = expected
        .offset_bytes
        .checked_add(expected.extent_length_bytes)
        .ok_or_else(|| StorageError::new("authorize volume extension", "volume end overflow"))?;
    if expected_end > snapshot.disk_size_bytes {
        return Err(StorageError::new(
            "authorize volume extension",
            "current volume extends beyond the current disk capacity",
        ));
    }
    let exact = snapshot
        .partitions
        .iter()
        .filter(|partition| {
            partition.offset_bytes == expected.offset_bytes
                && partition.size_bytes == expected.extent_length_bytes
        })
        .count();
    if exact != 1 {
        return Err(StorageError::new(
            "authorize volume extension",
            format!("expected exactly one canonical source partition, found {exact}"),
        ));
    }
    let mut authorized_end = snapshot.disk_size_bytes;
    for partition in &snapshot.partitions {
        if partition.offset_bytes == expected.offset_bytes
            && partition.size_bytes == expected.extent_length_bytes
        {
            continue;
        }
        let partition_end = partition
            .offset_bytes
            .checked_add(partition.size_bytes)
            .ok_or_else(|| {
                StorageError::new("authorize volume extension", "partition end overflow")
            })?;
        if expected.offset_bytes < partition_end && partition.offset_bytes < expected_end {
            return Err(StorageError::new(
                "authorize volume extension",
                "another canonical partition overlaps the current source volume",
            ));
        }
        if partition.offset_bytes >= expected_end {
            authorized_end = authorized_end.min(partition.offset_bytes);
        }
    }
    if authorized_end <= expected_end {
        return Err(StorageError::new(
            "authorize volume extension",
            "no canonical adjacent range remains after the current volume",
        ));
    }
    Ok(authorized_end)
}

/// Preserve the primary post-create failure while making rollback outcome explicit.
///
/// Partition creation is already committed when formatting, access-path assignment or the final
/// topology readback runs. Callers cannot safely reconstruct that partial state from a path, so
/// the shared boundary must either delete the exact provider-created extent itself or report that
/// the exact rollback failed.
fn require_post_create_or_rollback<T>(
    result: Result<T, StorageError>,
    rollback: impl FnOnce() -> Result<(), StorageError>,
) -> Result<T, StorageError> {
    match result {
        Ok(value) => Ok(value),
        Err(primary) => match rollback() {
            Ok(()) => Err(StorageError::new(
                primary.operation,
                format!(
                    "{}; exact provider-created partition was rolled back",
                    primary.detail
                ),
            )),
            Err(cleanup) => Err(StorageError::new(
                primary.operation,
                format!(
                    "{}; exact provider-created partition rollback failed: {}",
                    primary.detail, cleanup
                ),
            )),
        },
    }
}

/// GPT partition GUIDs are immutable identity tokens. MBR has no equivalent partition GUID on
/// every supported Windows version, so its partition number is retained only as probe diagnostics;
/// the stable match is the disk signature plus the exact physical extent checked above.
fn same_stable_partition_token(
    left: StablePartitionIdentity,
    right: StablePartitionIdentity,
) -> bool {
    match (left, right) {
        (
            StablePartitionIdentity::Gpt { partition_id: left },
            StablePartitionIdentity::Gpt {
                partition_id: right,
            },
        ) => left == right,
        (StablePartitionIdentity::Mbr { .. }, StablePartitionIdentity::Mbr { .. }) => true,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartitionRecord {
    pub partition_number: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub kind: PartitionKind,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageError {
    operation: &'static str,
    detail: String,
}

impl StorageError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl std::error::Error for StorageError {}

/// Hash a canonical IOCTL layout snapshot with an explicit version/domain separator.
///
/// The encoding is fixed-width and independent of Rust struct padding or VDS enumeration order.
pub fn disk_layout_snapshot_digest(snapshot: &DiskLayoutSnapshot) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"LetRecovery.DiskLayoutSnapshot.V2\0");
    hasher.update(snapshot.disk_size_bytes.to_le_bytes());
    match snapshot.disk {
        StableDiskIdentity::Raw => hasher.update([0]),
        StableDiskIdentity::Gpt { disk_id } => {
            hasher.update([2]);
            hasher.update(disk_id);
        }
        StableDiskIdentity::Mbr { signature } => {
            hasher.update([1]);
            hasher.update(signature.to_le_bytes());
        }
    }
    match snapshot.device_id_hash {
        Some(device_id) => {
            hasher.update([1]);
            hasher.update(device_id);
        }
        None => hasher.update([0]),
    }
    let mut partitions = snapshot.partitions.clone();
    partitions.sort();
    hasher.update((partitions.len() as u64).to_le_bytes());
    for partition in &partitions {
        hasher.update(partition.offset_bytes.to_le_bytes());
        hasher.update(partition.size_bytes.to_le_bytes());
        match partition.token {
            DiskLayoutPartitionToken::Gpt {
                partition_type,
                partition_id,
                attributes,
            } => {
                hasher.update([2]);
                hasher.update(partition_type);
                hasher.update(partition_id);
                hasher.update(attributes.to_le_bytes());
            }
            DiskLayoutPartitionToken::Mbr {
                partition_type,
                boot_indicator,
            } => {
                hasher.update([1, partition_type, u8::from(boot_indicator)]);
            }
        }
    }
    hasher.finalize().into()
}

pub fn validate_create_request(request: &CreatePartitionRequest) -> Result<(), StorageError> {
    if request.size_bytes == 0 {
        return Err(StorageError::new(
            "validate partition",
            "partition minimum capacity must be non-zero",
        ));
    }
    if request.offset_bytes != 0
        && request
            .offset_bytes
            .checked_add(request.size_bytes)
            .is_none()
    {
        return Err(StorageError::new(
            "validate partition",
            "partition minimum range overflows",
        ));
    }
    if request.label.encode_utf16().count() > 32 || request.label.contains(['\0', '\r', '\n']) {
        return Err(StorageError::new(
            "validate partition",
            "volume label is empty-invalid, too long, or contains a control character",
        ));
    }
    if let Some(letter) = request.drive_letter {
        let letter = letter.to_ascii_uppercase();
        if !('C'..='Z').contains(&letter) {
            return Err(StorageError::new(
                "validate partition",
                "drive letter must be in the C-Z range",
            ));
        }
    }
    match request.kind {
        PartitionKind::EfiSystem => {
            if request.file_system != Some(FileSystem::Fat32)
                || request.drive_letter.is_some()
                || request.active
            {
                return Err(StorageError::new(
                    "validate partition",
                    "EFI system partitions require FAT32, no drive letter, and no MBR active flag",
                ));
            }
        }
        PartitionKind::MicrosoftReserved => {
            if request.file_system.is_some() || request.drive_letter.is_some() || request.active {
                return Err(StorageError::new(
                    "validate partition",
                    "Microsoft reserved partitions cannot be formatted, mounted, or active",
                ));
            }
        }
        PartitionKind::Recovery => {
            if request.active {
                return Err(StorageError::new(
                    "validate partition",
                    "recovery partitions cannot be marked active",
                ));
            }
        }
        PartitionKind::BasicData => {}
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use std::mem::{size_of, ManuallyDrop};
    use std::sync::OnceLock;

    use windows::core::{
        w, IUnknown, Interface, BSTR, GUID, HRESULT, PCSTR, PCWSTR, PWSTR, VARIANT,
    };
    use windows::Win32::Foundation::{
        CloseHandle, BOOL, BOOLEAN, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_FUNCTION,
        ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, ERROR_NOT_SUPPORTED, ERROR_NO_MORE_FILES,
        E_INVALIDARG, E_UNEXPECTED, GENERIC_READ, HANDLE, RPC_E_CHANGED_MODE, RPC_E_TOO_LATE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, DefineDosDeviceW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose,
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
        QueryDosDeviceW, DDD_EXACT_MATCH_ON_REMOVE, DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::Storage::VirtualDiskService::{
        CLSID_VdsLoader, IEnumVdsObject, IVdsAdvancedDisk, IVdsAsync, IVdsCreatePartitionEx,
        IVdsDisk, IVdsDisk3, IVdsDiskPartitionMF, IVdsPack, IVdsService, IVdsServiceLoader,
        IVdsSwProvider, IVdsVolume, IVdsVolumeMF, IVdsVolumeMF2, IVdsVolumePlex, IVdsVolumeShrink,
        CHANGE_ATTRIBUTES_PARAMETERS, CHANGE_ATTRIBUTES_PARAMETERS_0,
        CHANGE_ATTRIBUTES_PARAMETERS_0_1, CREATE_PARTITION_PARAMETERS,
        CREATE_PARTITION_PARAMETERS_0, CREATE_PARTITION_PARAMETERS_0_0,
        CREATE_PARTITION_PARAMETERS_0_1, VDS_ASYNCOUT_CLEAN, VDS_ASYNCOUT_CREATEPARTITION,
        VDS_ASYNCOUT_EXTENDVOLUME, VDS_ASYNCOUT_FORMAT, VDS_ASYNCOUT_SHRINKVOLUME,
        VDS_ASYNC_OUTPUT, VDS_DET_FREE, VDS_DISK_EXTENT, VDS_DISK_FREE_EXTENT, VDS_DISK_PROP,
        VDS_DRIVE_LETTER_PROP, VDS_FST_EXFAT, VDS_FST_FAT, VDS_FST_FAT32, VDS_FST_NTFS,
        VDS_INPUT_DISK, VDS_OT_VOLUME, VDS_PARTITION_STYLE, VDS_PST_GPT, VDS_PST_MBR,
        VDS_QUERY_SOFTWARE_PROVIDERS, VDS_VOLUME_PLEX_PROP, VDS_VOLUME_PROP, VDS_VPT_SIMPLE,
    };
    use windows::Win32::System::Com::{
        CoCreateGuid, CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket,
        CoUninitialize, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, EOAC_NONE,
        RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
    };
    use windows::Win32::System::Ioctl::GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
    use windows::Win32::System::Wmi::{
        IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
        WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_FLAG_RETURN_WBEM_COMPLETE,
    };

    // Microsoft documents zero for both IVdsDisk3::QueryFreeExtents and
    // IVdsCreatePartitionEx::CreatePartitionEx as "use the provider's default alignment".
    const VDS_PROVIDER_DEFAULT_ALIGNMENT: u32 = 0;
    const GPT_BASIC_DATA: GUID = GUID::from_u128(0xebd0a0a2_b9e5_4433_87c0_68b6b72699c7);
    const GPT_ESP: GUID = GUID::from_u128(0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b);
    const GPT_MSR: GUID = GUID::from_u128(0xe3c9e316_0b5c_4db8_817d_f92df00215ae);
    const GPT_RECOVERY: GUID = GUID::from_u128(0xde94bba4_06d1_4d40_a16a_bfd50179d6ac);

    type CoTaskMemFreeFn = unsafe extern "system" fn(*const c_void);

    unsafe fn co_task_mem_free(pointer: *mut c_void) {
        if !pointer.is_null() {
            // Current Windows SDK import libraries redirect this legacy API through combase.dll,
            // which does not exist on an unmodified Windows 7 installation. Resolve the documented
            // ole32.dll export explicitly so the process loader never acquires a combase dependency.
            static FUNCTION: OnceLock<CoTaskMemFreeFn> = OnceLock::new();
            let function = FUNCTION.get_or_init(|| {
                let module = GetModuleHandleW(w!("ole32.dll"))
                    .expect("ole32.dll must be loaded before VDS returns COM memory");
                let address = GetProcAddress(module, PCSTR(c"CoTaskMemFree".as_ptr().cast()))
                    .expect("ole32.dll must export CoTaskMemFree on supported Windows versions");
                std::mem::transmute::<unsafe extern "system" fn() -> isize, CoTaskMemFreeFn>(
                    address,
                )
            });
            function(pointer.cast_const());
        }
    }

    struct ComApartment {
        uninitialize: bool,
    }

    struct VolumeSearchHandle(Option<HANDLE>);

    impl VolumeSearchHandle {
        fn raw(&self) -> HANDLE {
            self.0.expect("volume search handle is open")
        }
    }

    impl Drop for VolumeSearchHandle {
        fn drop(&mut self) {
            if let Some(handle) = self.0.take() {
                unsafe {
                    let _ = FindVolumeClose(handle);
                }
            }
        }
    }

    impl ComApartment {
        unsafe fn enter() -> Result<Self, StorageError> {
            let result = CoInitializeEx(None, COINIT_MULTITHREADED);
            if result.is_ok() {
                return Ok(Self { uninitialize: true });
            }
            if result == RPC_E_CHANGED_MODE {
                return Ok(Self {
                    uninitialize: false,
                });
            }
            Err(hresult_error("initialize COM", result))
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.uninitialize {
                unsafe { CoUninitialize() };
            }
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn validated_physical_disk_device_number(
        device_type: u32,
        device_number: u32,
        source: &'static str,
    ) -> Result<Option<(u32, &'static str)>, StorageError> {
        use windows::Win32::Storage::FileSystem::FILE_DEVICE_DISK;

        // STORAGE_DEVICE_NUMBER is a (DeviceType, DeviceNumber) identity. DeviceNumber alone is
        // not a physical-disk identity: for example, CD-ROM 0 and hard disk 0 can coexist. This is
        // especially important for VDS, whose disk enumeration also includes optical media and
        // whose VDS_DISK_PROP explicitly reports FILE_DEVICE_CD_ROM/FILE_DEVICE_DVD.
        if device_type != FILE_DEVICE_DISK.0 {
            return Err(StorageError::new(
                "resolve opened physical disk number",
                format!(
                    "{source} returned device type {device_type:#010x} for device number {device_number}; expected FILE_DEVICE_DISK ({:#010x})",
                    FILE_DEVICE_DISK.0
                ),
            ));
        }
        if device_number == u32::MAX {
            // Microsoft documents 0xFFFFFFFF for an MPIO physical path. It is not a usable
            // current disk number and must not be guessed from the symbolic interface path.
            return Ok(None);
        }
        Ok(Some((device_number, source)))
    }

    pub unsafe fn query_present_disk_device_number(
        handle: HANDLE,
    ) -> Result<Option<(u32, &'static str)>, StorageError> {
        use windows::Win32::System::Ioctl::{
            IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_GET_DEVICE_NUMBER_EX,
            STORAGE_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER_EX,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        let mut legacy = STORAGE_DEVICE_NUMBER::default();
        let mut returned = 0_u32;
        let legacy_result = match DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some((&mut legacy as *mut STORAGE_DEVICE_NUMBER).cast()),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut returned),
            None,
        ) {
            Ok(()) if returned >= size_of::<STORAGE_DEVICE_NUMBER>() as u32 => {
                validated_physical_disk_device_number(
                    legacy.DeviceType,
                    legacy.DeviceNumber,
                    "IOCTL_STORAGE_GET_DEVICE_NUMBER",
                )
            }
            Ok(()) => Err(StorageError::new(
                "read opened disk path legacy number",
                format!(
                    "truncated STORAGE_DEVICE_NUMBER response: returned={returned} required={}",
                    size_of::<STORAGE_DEVICE_NUMBER>()
                ),
            )),
            Err(error) => Err(api_error("read opened disk path legacy number", error)),
        };

        resolve_device_number_with_extended_fallback(legacy_result, || {
            // The extended IOCTL is issued only after the Vista/Windows 7 legacy request. It is an
            // IOCTL value rather than a loader import, so old kernels simply reject it at runtime.
            let mut extended = STORAGE_DEVICE_NUMBER_EX::default();
            let mut extended_returned = 0_u32;
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER_EX,
                None,
                0,
                Some((&mut extended as *mut STORAGE_DEVICE_NUMBER_EX).cast()),
                size_of::<STORAGE_DEVICE_NUMBER_EX>() as u32,
                Some(&mut extended_returned),
                None,
            )
            .map_err(|error| api_error("read opened disk path extended number", error))?;
            if extended_returned < size_of::<STORAGE_DEVICE_NUMBER_EX>() as u32
                || extended.Version < size_of::<STORAGE_DEVICE_NUMBER_EX>() as u32
                || extended.Size < size_of::<STORAGE_DEVICE_NUMBER_EX>() as u32
            {
                return Err(StorageError::new(
                    "read opened disk path extended number",
                    "invalid or path-only STORAGE_DEVICE_NUMBER_EX response",
                ));
            }
            validated_physical_disk_device_number(
                extended.DeviceType,
                extended.DeviceNumber,
                "IOCTL_STORAGE_GET_DEVICE_NUMBER_EX",
            )
        })
    }

    /// Enumerate only disk interfaces currently reported as present by SetupAPI.
    ///
    /// The opaque interface path and its current disk number stay paired. Inventory callers must
    /// issue capacity and layout IOCTLs through this exact path instead of reopening a potentially
    /// different `PhysicalDriveN` alias exposed by a vendor filter stack.
    pub unsafe fn present_physical_disk_interfaces(
    ) -> Result<Vec<PresentDiskInterface>, StorageError> {
        use windows::Win32::Devices::DeviceAndDriverInstallation::{
            SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
            SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
            SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        };
        use windows::Win32::Foundation::{GetLastError, ERROR_NO_MORE_ITEMS, HWND};
        use windows::Win32::System::Ioctl::GUID_DEVINTERFACE_DISK;

        let set = SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_DISK),
            PCWSTR::null(),
            HWND::default(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|error| api_error("enumerate present disk interfaces", error))?;

        let result = (|| -> Result<Vec<PresentDiskInterface>, StorageError> {
            let mut interfaces = Vec::new();
            let mut index = 0_u32;
            loop {
                let mut interface = SP_DEVICE_INTERFACE_DATA {
                    cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                    ..Default::default()
                };
                if let Err(error) = SetupDiEnumDeviceInterfaces(
                    set,
                    None,
                    &GUID_DEVINTERFACE_DISK,
                    index,
                    &mut interface,
                ) {
                    if GetLastError() == ERROR_NO_MORE_ITEMS {
                        break;
                    }
                    return Err(api_error("enumerate present disk interface", error));
                }
                index = index.saturating_add(1);

                let mut required = 0_u32;
                let _ = SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &interface,
                    None,
                    0,
                    Some(&mut required),
                    None,
                );
                if required < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
                    log::warn!(
                        "SetupAPI disk interface {} returned invalid detail size {} and was skipped",
                        index - 1,
                        required
                    );
                    continue;
                }
                let mut storage = vec![0_usize; (required as usize).div_ceil(size_of::<usize>())];
                let detail = storage
                    .as_mut_ptr()
                    .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
                (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if let Err(error) = SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &interface,
                    Some(detail),
                    required,
                    None,
                    None,
                ) {
                    log::warn!(
                        "SetupAPI disk interface {} detail query failed and was skipped: {}",
                        index - 1,
                        error
                    );
                    continue;
                }
                let path = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
                let device_path = match PCWSTR(path).to_string() {
                    Ok(path) => path,
                    Err(error) => {
                        log::warn!(
                            "SetupAPI disk interface {} path decoding failed and was skipped: {}",
                            index - 1,
                            error
                        );
                        continue;
                    }
                };

                let zero_access = CreateFileW(
                    PCWSTR(path),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    Default::default(),
                    None,
                )
                .map(OwnedHandle);
                let resolved = match zero_access {
                    Ok(zero_access) => match query_present_disk_device_number(zero_access.0) {
                        Ok(value) => Ok(value),
                        Err(zero_error) => {
                            match CreateFileW(
                                PCWSTR(path),
                                GENERIC_READ.0,
                                FILE_SHARE_READ | FILE_SHARE_WRITE,
                                None,
                                OPEN_EXISTING,
                                Default::default(),
                                None,
                            )
                            .map(OwnedHandle) {
                                Ok(read) => query_present_disk_device_number(read.0).map_err(|read_error| {
                                    StorageError::new(
                                        "read present disk interface number",
                                        format!(
                                            "zero-access query failed ({zero_error}); read-only query failed ({read_error})"
                                        ),
                                    )
                                }),
                                Err(read_error) => Err(StorageError::new(
                                    "read present disk interface number",
                                    format!(
                                        "zero-access query failed ({zero_error}); read-only open failed ({read_error})"
                                    ),
                                )),
                            }
                        }
                    },
                    Err(zero_open_error) => {
                        match CreateFileW(
                            PCWSTR(path),
                            GENERIC_READ.0,
                            FILE_SHARE_READ | FILE_SHARE_WRITE,
                            None,
                            OPEN_EXISTING,
                            Default::default(),
                            None,
                        )
                        .map(OwnedHandle) {
                            Ok(read) => query_present_disk_device_number(read.0).map_err(|read_error| {
                                StorageError::new(
                                    "read present disk interface number",
                                    format!(
                                        "zero-access open failed ({zero_open_error}); read-only query failed ({read_error})"
                                    ),
                                )
                            }),
                            Err(read_error) => Err(StorageError::new(
                                "read present disk interface number",
                                format!(
                                    "zero-access open failed ({zero_open_error}); read-only open failed ({read_error})"
                                ),
                            )),
                        }
                    }
                };
                let resolved = match resolved {
                    Ok(value) => value,
                    Err(error) => {
                        log::warn!(
                            "SetupAPI disk interface {} could not provide a current disk number and was skipped: {}",
                            index - 1,
                            error
                        );
                        continue;
                    }
                };
                let Some((number, query)) = resolved else {
                    log::warn!(
                        "SetupAPI disk interface {} is an MPIO physical path without a usable current disk number; skipped",
                        index - 1
                    );
                    continue;
                };
                log::debug!(
                    "SetupAPI disk interface {} resolved to current disk {} using {}",
                    index - 1,
                    number,
                    query
                );
                interfaces.push(PresentDiskInterface {
                    disk_number: number,
                    device_path,
                });
            }
            interfaces.sort_by(|left, right| {
                left.disk_number
                    .cmp(&right.disk_number)
                    .then_with(|| left.device_path.cmp(&right.device_path))
            });
            interfaces.dedup_by(|left, right| {
                left.disk_number == right.disk_number && left.device_path == right.device_path
            });
            if interfaces.is_empty() {
                return Err(StorageError::new(
                    "enumerate present disk interfaces",
                    "SetupAPI returned no present physical disks",
                ));
            }
            Ok(interfaces)
        })();

        if let Err(error) = SetupDiDestroyDeviceInfoList(set) {
            // SetupDiDestroyDeviceInfoList only releases the in-memory device information set.
            // Once enumeration completed, a cleanup failure does not invalidate the disk numbers
            // already returned by SetupAPI and must not turn a safe, read-only inventory into an
            // installation-blocking false negative.
            log::warn!("SetupAPI present-disk interface set cleanup failed: {error}");
        }
        result
    }

    pub unsafe fn present_physical_disk_numbers() -> Result<Vec<u32>, StorageError> {
        present_physical_disk_interfaces().map(|interfaces| {
            let numbers = interfaces
                .into_iter()
                .map(|interface| interface.disk_number)
                .collect::<Vec<_>>();
            sort_dedup_physical_disk_numbers(numbers)
        })
    }

    unsafe fn open_present_disk_interface_path(
        path: &str,
        access: u32,
        operation: &'static str,
    ) -> Result<OwnedHandle, StorageError> {
        let path = wide(path);
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| api_error(operation, error))
    }

    unsafe fn verify_opened_present_disk_number(
        handle: HANDLE,
        expected_disk_number: u32,
        operation: &'static str,
    ) -> Result<(), StorageError> {
        let Some((actual, source)) = query_present_disk_device_number(handle)? else {
            return Err(StorageError::new(
                operation,
                "UntrustedStorage: opened disk interface is an MPIO path without a usable current disk number",
            ));
        };
        if actual != expected_disk_number {
            return Err(StorageError::new(
                operation,
                format!(
                    "UntrustedStorage: opened disk interface changed from current disk {expected_disk_number} to {actual} while binding it through {source}"
                ),
            ));
        }
        Ok(())
    }

    /// Read the complete canonical observation through every opaque SetupAPI path currently bound
    /// to `disk_number`. This deliberately does not manufacture or reopen a `PhysicalDriveN`
    /// alias. Multiple aliases may agree; contradictory aliases make the disk untrusted.
    unsafe fn trusted_present_disk_snapshot(
        disk_number: u32,
    ) -> Result<(String, DiskLayoutSnapshot), StorageError> {
        let interfaces = present_physical_disk_interfaces()?
            .into_iter()
            .filter(|interface| interface.disk_number == disk_number)
            .collect::<Vec<_>>();
        let mut observations = Vec::with_capacity(interfaces.len());
        for interface in interfaces {
            let handle = open_present_disk_interface_path(
                &interface.device_path,
                GENERIC_READ.0,
                "open present disk interface for canonical snapshot",
            )?;
            verify_opened_present_disk_number(
                handle.0,
                disk_number,
                "verify present disk interface for canonical snapshot",
            )?;
            let snapshot = disk_layout_snapshot_from_handle(handle.0)?;
            observations.push((interface.device_path, snapshot));
        }
        let reconciled = reconcile_present_disk_snapshots(disk_number, observations)?;
        if reconciled.1.device_id_hash.is_none() {
            warn_missing_device_id_once(disk_number);
        }
        Ok(reconciled)
    }

    fn warn_missing_device_id_once(disk_number: u32) {
        static WARNED_DISKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u32>>> =
            std::sync::OnceLock::new();
        let warned_disks =
            WARNED_DISKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        let mut warned_disks = match warned_disks.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if warned_disks.insert(disk_number) {
            log::warn!(
                "physical disk {disk_number} exposes no optional StorageIdAssocDevice identifier; replacement detection continues with same-handle capacity, GPT/MBR partition-table token and exact extents"
            );
        } else {
            log::debug!(
                "physical disk {disk_number} still exposes no optional StorageIdAssocDevice identifier"
            );
        }
    }

    /// Open the already reconciled opaque disk path, then repeat the current-number and complete
    /// canonical observation checks on the exact handle that will be used by the caller.
    unsafe fn open_trusted_present_disk(
        disk_number: u32,
        access: u32,
        operation: &'static str,
    ) -> Result<(OwnedHandle, DiskLayoutSnapshot), StorageError> {
        let (path, trusted) = trusted_present_disk_snapshot(disk_number)?;
        let handle = open_present_disk_interface_path(&path, access | GENERIC_READ.0, operation)?;
        verify_opened_present_disk_number(handle.0, disk_number, operation)?;
        let rebound = disk_layout_snapshot_from_handle(handle.0)?;
        let (_, rebound) = reconcile_present_disk_snapshots(
            disk_number,
            vec![("prior consensus".to_owned(), trusted), (path, rebound)],
        )?;
        Ok((handle, rebound))
    }

    struct Vds {
        // Rust drops fields in declaration order. Release every COM interface before calling
        // CoUninitialize through the apartment guard; reversing these fields can dispatch a COM
        // Release after the apartment has already been torn down.
        service: IVdsService,
        _apartment: ComApartment,
    }

    struct DiskObject {
        disk: IVdsDisk,
        id: GUID,
        style: VDS_PARTITION_STYLE,
        size_bytes: u64,
    }

    unsafe fn exact_com_interface<T: Interface>(
        operation: &'static str,
        result: HRESULT,
        raw: *mut c_void,
    ) -> Result<T, StorageError> {
        if let Err(error) = require_exact_success(operation, result) {
            if !raw.is_null() {
                drop(IUnknown::from_raw(raw));
            }
            return Err(error);
        }
        if raw.is_null() {
            return Err(StorageError::new(
                operation,
                "VDS returned a null interface",
            ));
        }
        Ok(T::from_raw(raw))
    }

    unsafe fn exact_async_interface(
        operation: &'static str,
        result: HRESULT,
        raw: *mut c_void,
    ) -> Result<IVdsAsync, StorageError> {
        exact_com_interface(operation, result, raw)
    }

    unsafe fn partition_create_async_interface(
        result: HRESULT,
        raw: *mut c_void,
    ) -> Result<(IVdsAsync, Option<HRESULT>), StorageError> {
        let warning = if result == HRESULT(0) {
            None
        } else if result == VDS_S_UPDATE_BOOTFILE_FAILED_HRESULT {
            Some(result)
        } else {
            if !raw.is_null() {
                drop(IUnknown::from_raw(raw));
            }
            return Err(hresult_error("start VDS partition creation", result));
        };
        if raw.is_null() {
            return Err(StorageError::new(
                "start VDS partition creation",
                "VDS returned success without the documented asynchronous interface",
            ));
        }
        Ok((IVdsAsync::from_raw(raw), warning))
    }

    impl Vds {
        unsafe fn connect() -> Result<Self, StorageError> {
            let apartment = ComApartment::enter()?;
            let loader: IVdsServiceLoader =
                CoCreateInstance(&CLSID_VdsLoader, None, CLSCTX_LOCAL_SERVER)
                    .map_err(|error| api_error("create VDS loader", error))?;
            let service = loader
                .LoadService(PCWSTR::null())
                .map_err(|error| api_error("load VDS service", error))?;
            service
                .WaitForServiceReady()
                .map_err(|error| api_error("wait for VDS service", error))?;
            Ok(Self {
                service,
                _apartment: apartment,
            })
        }

        unsafe fn refresh(&self) -> Result<(), StorageError> {
            let reenumerate =
                (Interface::vtable(&self.service).Reenumerate)(Interface::as_raw(&self.service));
            require_exact_success("re-enumerate VDS", reenumerate)?;
            let refresh =
                (Interface::vtable(&self.service).Refresh)(Interface::as_raw(&self.service));
            require_exact_success("refresh VDS", refresh)
        }

        unsafe fn providers(&self) -> Result<Vec<IVdsSwProvider>, StorageError> {
            let mut raw = std::ptr::null_mut();
            let result = (Interface::vtable(&self.service).QueryProviders)(
                Interface::as_raw(&self.service),
                VDS_QUERY_SOFTWARE_PROVIDERS.0 as u32,
                &mut raw,
            );
            let enumerator: IEnumVdsObject =
                exact_com_interface("enumerate VDS providers", result, raw)?;
            enum_objects(&enumerator)?
                .into_iter()
                .map(|object| {
                    object
                        .cast::<IVdsSwProvider>()
                        .map_err(|error| api_error("open VDS software provider", error))
                })
                .collect()
        }

        unsafe fn packs(&self) -> Result<Vec<IVdsPack>, StorageError> {
            let mut result = Vec::new();
            for provider in self.providers()? {
                let mut raw = std::ptr::null_mut();
                let query_result = (Interface::vtable(&provider).QueryPacks)(
                    Interface::as_raw(&provider),
                    &mut raw,
                );
                let enumerator: IEnumVdsObject =
                    exact_com_interface("enumerate VDS packs", query_result, raw)?;
                for object in enum_objects(&enumerator)? {
                    result.push(
                        object
                            .cast::<IVdsPack>()
                            .map_err(|error| api_error("open VDS pack", error))?,
                    );
                }
            }
            Ok(result)
        }

        unsafe fn volumes(&self) -> Result<Vec<IVdsVolume>, StorageError> {
            let mut result = Vec::new();
            for pack in self.packs()? {
                let mut raw = std::ptr::null_mut();
                let query_result =
                    (Interface::vtable(&pack).QueryVolumes)(Interface::as_raw(&pack), &mut raw);
                let enumerator: IEnumVdsObject =
                    exact_com_interface("enumerate VDS volumes", query_result, raw)?;
                for object in enum_objects(&enumerator)? {
                    result.push(
                        object
                            .cast::<IVdsVolume>()
                            .map_err(|error| api_error("open VDS volume", error))?,
                    );
                }
            }
            Ok(result)
        }

        unsafe fn find_disk(&self, disk_number: u32) -> Result<DiskObject, StorageError> {
            for pack in self.packs()? {
                let mut raw = std::ptr::null_mut();
                let query_result =
                    (Interface::vtable(&pack).QueryDisks)(Interface::as_raw(&pack), &mut raw);
                let enumerator: IEnumVdsObject =
                    exact_com_interface("enumerate VDS disks", query_result, raw)?;
                for object in enum_objects(&enumerator)? {
                    if let Some(disk) = disk_object_if_number(object, disk_number)? {
                        return Ok(disk);
                    }
                }
            }
            // A successful Clean removes the disk from its provider pack. Microsoft requires
            // QueryUnallocatedDisks for these raw/uninitialized disk objects; pack-only lookup
            // would otherwise lose the target after it has already been destructively cleaned.
            let mut raw = std::ptr::null_mut();
            let result = (Interface::vtable(&self.service).QueryUnallocatedDisks)(
                Interface::as_raw(&self.service),
                &mut raw,
            );
            let enumerator: IEnumVdsObject =
                exact_com_interface("enumerate unallocated VDS disks", result, raw)?;
            for object in enum_objects(&enumerator)? {
                if let Some(disk) = disk_object_if_number(object, disk_number)? {
                    return Ok(disk);
                }
            }
            Err(StorageError::new(
                "find disk",
                format!("physical disk {disk_number} was not found by VDS"),
            ))
        }

        unsafe fn find_disk_in_pack(
            &self,
            pack: &IVdsPack,
            disk_number: u32,
        ) -> Result<DiskObject, StorageError> {
            let mut raw = std::ptr::null_mut();
            let query_result =
                (Interface::vtable(pack).QueryDisks)(Interface::as_raw(pack), &mut raw);
            let enumerator: IEnumVdsObject =
                exact_com_interface("enumerate disks in VDS volume pack", query_result, raw)?;
            let mut selected = None;
            for object in enum_objects(&enumerator)? {
                if let Some(disk) = disk_object_if_number(object, disk_number)? {
                    if selected.is_some() {
                        return Err(StorageError::new(
                            "bind disk in VDS volume pack",
                            format!(
                                "volume pack returned more than one disk object for physical disk {disk_number}"
                            ),
                        ));
                    }
                    selected = Some(disk);
                }
            }
            selected.ok_or_else(|| {
                StorageError::new(
                    "bind disk in VDS volume pack",
                    format!(
                        "the volume's own VDS pack contains no disk object for physical disk {disk_number}"
                    ),
                )
            })
        }

        unsafe fn find_disk_for_hidden_partition(
            &self,
            disk_number: u32,
            expected_partition: DiskLayoutPartitionSnapshot,
        ) -> Result<IVdsAdvancedDisk, StorageError> {
            let mut matches = Vec::new();
            let mut unreadable = 0_usize;
            let mut first_error = None;
            let mut note_error = |error: StorageError| {
                unreadable = unreadable.saturating_add(1);
                if first_error.is_none() {
                    first_error = Some(error.to_string().chars().take(512).collect::<String>());
                }
            };

            // Microsoft requires callers that want a complete disk view to query across every
            // software provider. WinPE can consequently return multiple VDS aliases for the same
            // canonical PhysicalDrive. Bind each alias back through its opaque locator, then
            // require GetPartitionProperties to recognize the exact current GPT ESP offset before
            // it becomes usable. Multiple aliases that pass those checks still name one physical
            // disk+partition and are environment noise, not ambiguous destructive targets.
            let mut provider_raw = std::ptr::null_mut();
            let provider_result = (Interface::vtable(&self.service).QueryProviders)(
                Interface::as_raw(&self.service),
                VDS_QUERY_SOFTWARE_PROVIDERS.0 as u32,
                &mut provider_raw,
            );
            let providers: IEnumVdsObject =
                exact_com_interface("enumerate VDS providers", provider_result, provider_raw)?;
            for provider_object in enum_objects(&providers)? {
                let provider = match provider_object.cast::<IVdsSwProvider>() {
                    Ok(provider) => provider,
                    Err(error) => {
                        note_error(api_error("open unrelated VDS software provider", error));
                        continue;
                    }
                };
                let mut pack_raw = std::ptr::null_mut();
                let pack_result = (Interface::vtable(&provider).QueryPacks)(
                    Interface::as_raw(&provider),
                    &mut pack_raw,
                );
                let packs: IEnumVdsObject = match exact_com_interface(
                    "enumerate VDS packs for hidden partition",
                    pack_result,
                    pack_raw,
                ) {
                    Ok(packs) => packs,
                    Err(error) => {
                        note_error(error);
                        continue;
                    }
                };
                for pack_object in enum_objects(&packs)? {
                    let pack = match pack_object.cast::<IVdsPack>() {
                        Ok(pack) => pack,
                        Err(error) => {
                            note_error(api_error("open unrelated VDS pack", error));
                            continue;
                        }
                    };
                    let mut disk_raw = std::ptr::null_mut();
                    let disk_result = (Interface::vtable(&pack).QueryDisks)(
                        Interface::as_raw(&pack),
                        &mut disk_raw,
                    );
                    let disks: IEnumVdsObject = match exact_com_interface(
                        "enumerate VDS disks for hidden partition",
                        disk_result,
                        disk_raw,
                    ) {
                        Ok(disks) => disks,
                        Err(error) => {
                            note_error(error);
                            continue;
                        }
                    };
                    for disk_object in enum_objects(&disks)? {
                        match disk_object_if_number(disk_object, disk_number) {
                            Ok(Some(disk)) => {
                                match bind_hidden_partition_candidate(disk, expected_partition) {
                                    Ok(binding) => matches.push(binding),
                                    Err(error) => note_error(error),
                                }
                            }
                            Ok(None) => {}
                            Err(error) => note_error(error),
                        }
                    }
                }
            }

            if unreadable != 0 {
                log::debug!(
                    "ignored {unreadable} rejected or unreadable VDS provider/pack/disk object(s) while resolving hidden-partition disk {disk_number}; first error: {}",
                    first_error.as_deref().unwrap_or("diagnostic unavailable")
                );
            }
            finish_hidden_disk_search(
                matches,
                disk_number,
                expected_partition.offset_bytes,
                unreadable,
                first_error.as_deref(),
            )
        }

        unsafe fn find_volume_by_letter(
            &self,
            drive_letter: char,
        ) -> Result<IVdsVolume, StorageError> {
            let letter = normalize_letter(drive_letter)?;
            let mut properties = [VDS_DRIVE_LETTER_PROP::default(); 26];
            let query = (Interface::vtable(&self.service).QueryDriveLetters)(
                Interface::as_raw(&self.service),
                'A' as u16,
                properties.len() as u32,
                properties.as_mut_ptr(),
            );
            require_exact_success("query VDS drive letters", query)?;
            let property = properties
                .iter()
                .find(|property| property.bUsed.as_bool() && property.wcLetter == letter as u16)
                .ok_or_else(|| {
                    StorageError::new(
                        "find volume",
                        format!("drive letter {letter}: is not assigned to a VDS volume"),
                    )
                })?;
            let mut raw = std::ptr::null_mut();
            let open = (Interface::vtable(&self.service).GetObject)(
                Interface::as_raw(&self.service),
                property.volumeId,
                VDS_OT_VOLUME,
                &mut raw,
            );
            let object: IUnknown = exact_com_interface("open VDS volume", open, raw)?;
            object
                .cast::<IVdsVolume>()
                .map_err(|error| api_error("query VDS volume interface", error))
        }

        unsafe fn find_volume_by_id(&self, volume_id: GUID) -> Result<IVdsVolume, StorageError> {
            if volume_id == GUID::zeroed() {
                return Err(StorageError::new(
                    "find volume",
                    "the provider did not associate a volume with the created partition",
                ));
            }
            let mut raw = std::ptr::null_mut();
            let open = (Interface::vtable(&self.service).GetObject)(
                Interface::as_raw(&self.service),
                volume_id,
                VDS_OT_VOLUME,
                &mut raw,
            );
            let object: IUnknown = exact_com_interface("open created VDS volume", open, raw)?;
            object
                .cast::<IVdsVolume>()
                .map_err(|error| api_error("query created VDS volume interface", error))
        }
    }

    unsafe fn bind_hidden_partition_candidate(
        disk: DiskObject,
        expected: DiskLayoutPartitionSnapshot,
    ) -> Result<(GUID, IVdsAdvancedDisk), StorageError> {
        let expected_type = match expected.token {
            DiskLayoutPartitionToken::Gpt { partition_type, .. }
                if partition_type == guid_identity(GPT_ESP) =>
            {
                GPT_ESP
            }
            _ => {
                return Err(StorageError::new(
                    "bind hidden partition candidate",
                    "the canonical target is not a GPT EFI system partition",
                ));
            }
        };
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open candidate VDS advanced disk", error))?;
        let mut properties =
            windows::Win32::Storage::VirtualDiskService::VDS_PARTITION_PROP::default();
        let result = (Interface::vtable(&advanced).GetPartitionProperties)(
            Interface::as_raw(&advanced),
            expected.offset_bytes,
            &mut properties,
        );
        require_exact_success("query candidate hidden partition", result)?;
        if !vds_hidden_partition_matches(
            expected.offset_bytes,
            expected_type,
            properties.ullOffset,
            properties.PartitionStyle,
            properties.Anonymous.Gpt.partitionType,
        ) {
            return Err(StorageError::new(
                "bind hidden partition candidate",
                "VDS candidate does not expose the canonical GPT ESP at the exact byte offset",
            ));
        }
        Ok((disk.id, advanced))
    }

    fn vds_hidden_partition_matches(
        expected_offset: u64,
        expected_type: GUID,
        observed_offset: u64,
        observed_style: VDS_PARTITION_STYLE,
        observed_type: GUID,
    ) -> bool {
        observed_offset == expected_offset
            && observed_style == VDS_PST_GPT
            && observed_type == expected_type
    }

    fn finish_hidden_disk_search<T>(
        matches: Vec<(GUID, T)>,
        disk_number: u32,
        offset_bytes: u64,
        unreadable: usize,
        first_error: Option<&str>,
    ) -> Result<T, StorageError> {
        if matches.is_empty() {
            return Err(StorageError::new(
                "find disk for hidden partition",
                format!(
                    "no VDS disk alias exposes the canonical GPT ESP on physical disk {disk_number} at offset {offset_bytes}; rejected/unreadable object count={unreadable}; first error={}",
                    first_error.unwrap_or("none")
                ),
            ));
        }
        if matches.len() > 1 {
            let mut unique_ids = Vec::new();
            for (id, _) in &matches {
                if !unique_ids.contains(id) {
                    unique_ids.push(*id);
                }
            }
            log::info!(
                "physical disk {disk_number} GPT ESP offset {offset_bytes} has {} usable VDS alias(es), {} unique session object id(s); using the first exact alias",
                matches.len(),
                unique_ids.len()
            );
        }
        Ok(matches
            .into_iter()
            .next()
            .expect("non-empty checked hidden-partition alias set")
            .1)
    }

    unsafe fn enum_objects(enumerator: &IEnumVdsObject) -> Result<Vec<IUnknown>, StorageError> {
        let mut result = Vec::new();
        loop {
            let mut values: [Option<IUnknown>; 1] = [None];
            let mut fetched = 0;
            enumerator
                .Next(&mut values, &mut fetched)
                .map_err(|error| api_error("enumerate VDS object", error))?;
            if fetched == 0 {
                break;
            }
            let value = values[0]
                .take()
                .ok_or_else(|| StorageError::new("enumerate VDS object", "null object"))?;
            result.push(value);
        }
        Ok(result)
    }

    unsafe fn disk_object_if_number(
        object: IUnknown,
        disk_number: u32,
    ) -> Result<Option<DiskObject>, StorageError> {
        let disk = object
            .cast::<IVdsDisk>()
            .map_err(|error| api_error("open VDS disk", error))?;
        let mut properties = VDS_DISK_PROP::default();
        let result =
            (Interface::vtable(&disk).GetProperties)(Interface::as_raw(&disk), &mut properties);
        if let Err(error) = validate_vds_disk_locator_result(result) {
            free_disk_properties(&mut properties);
            return Err(error);
        }
        use windows::Win32::Storage::FileSystem::FILE_DEVICE_DISK;
        if properties.dwDeviceType != FILE_DEVICE_DISK.0 {
            log::debug!(
                "ignoring non-disk VDS object: device_type={:#010x} device_number_paths_are_not_physical_disk_locators",
                properties.dwDeviceType
            );
            free_disk_properties(&mut properties);
            return Ok(None);
        }
        let number = disk_number_from_properties(&properties);
        let id = properties.id;
        let style = properties.PartitionStyle;
        let size_bytes = properties.ullSize;
        free_disk_properties(&mut properties);
        let number = number?;
        Ok((number == Some(disk_number)).then_some(DiskObject {
            disk,
            id,
            style,
            size_bytes,
        }))
    }

    fn sort_dedup_physical_disk_numbers(mut numbers: Vec<u32>) -> Vec<u32> {
        numbers.sort_unstable();
        numbers.dedup();
        numbers
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum AsyncWarningPolicy {
        Exact,
        Clean,
        Shrink,
        CreatePartition,
    }

    const VDS_S_DISK_PARTIALLY_CLEANED_HRESULT: HRESULT = HRESULT(0x0004_241A);
    const VDS_S_NO_NOTIFICATION_HRESULT: HRESULT = HRESULT(0x0004_2517);
    const VDS_S_PROPERTIES_INCOMPLETE_HRESULT: HRESULT = HRESULT(0x0004_2715);
    const VDS_S_UPDATE_BOOTFILE_FAILED_HRESULT: HRESULT = HRESULT(0x0004_2434);
    const S_FALSE_HRESULT: HRESULT = HRESULT(1);

    fn validate_async_result(
        operation: &'static str,
        result: HRESULT,
        policy: AsyncWarningPolicy,
    ) -> Result<Option<HRESULT>, StorageError> {
        if result == HRESULT(0) {
            return Ok(None);
        }
        let allowed = matches!(
            (policy, result),
            (
                AsyncWarningPolicy::Clean,
                VDS_S_DISK_PARTIALLY_CLEANED_HRESULT
            ) | (AsyncWarningPolicy::Shrink, VDS_S_NO_NOTIFICATION_HRESULT)
                | (
                    AsyncWarningPolicy::CreatePartition,
                    VDS_S_UPDATE_BOOTFILE_FAILED_HRESULT
                )
        );
        if allowed {
            Ok(Some(result))
        } else {
            Err(hresult_error(operation, result))
        }
    }

    unsafe fn wait_async_with_policy(
        operation: &'static str,
        asynchronous: &IVdsAsync,
        expected_type: Option<windows::Win32::Storage::VirtualDiskService::VDS_ASYNC_OUTPUT_TYPE>,
        policy: AsyncWarningPolicy,
    ) -> Result<(VDS_ASYNC_OUTPUT, Option<HRESULT>), StorageError> {
        let mut result = HRESULT(0);
        let mut output = VDS_ASYNC_OUTPUT::default();
        let wait_result = (Interface::vtable(asynchronous).Wait)(
            Interface::as_raw(asynchronous),
            &mut result,
            &mut output,
        );
        require_exact_success(operation, wait_result)?;
        let warning = validate_async_result(operation, result, policy)?;
        if let Some(expected) = expected_type {
            if output.r#type != expected {
                return Err(hresult_error(operation, E_UNEXPECTED));
            }
        }
        Ok((output, warning))
    }

    unsafe fn wait_async(
        operation: &'static str,
        asynchronous: &IVdsAsync,
        expected_type: Option<windows::Win32::Storage::VirtualDiskService::VDS_ASYNC_OUTPUT_TYPE>,
    ) -> Result<VDS_ASYNC_OUTPUT, StorageError> {
        wait_async_with_policy(
            operation,
            asynchronous,
            expected_type,
            AsyncWarningPolicy::Exact,
        )
        .map(|(output, _)| output)
    }

    unsafe fn disk_number_from_properties(
        properties: &VDS_DISK_PROP,
    ) -> Result<Option<u32>, StorageError> {
        // Microsoft documents pwszName as a CreateFileW-ready disk name and pwszDevicePath as a
        // Plug and Play path. SetupDiGetDeviceInterfaceDetailW explicitly says that symbolic device
        // paths are opaque and must not be parsed. Open each returned path as-is and let the storage
        // stack authoritatively report the current disk number through
        // IOCTL_STORAGE_GET_DEVICE_NUMBER (Windows XP+, therefore available on Windows 7).
        let mut errors = Vec::new();
        for (label, value) in [
            ("VDS disk name", properties.pwszName),
            ("VDS disk device path", properties.pwszDevicePath),
        ] {
            if value.is_null() {
                continue;
            }
            match disk_number_from_opaque_path(value, label) {
                Ok(Some(number)) => return Ok(Some(number)),
                Ok(None) => errors.push(format!("{label}: MPIO path has no usable disk number")),
                Err(error) => errors.push(format!("{label}: {error}")),
            }
        }
        if errors.is_empty() {
            Err(StorageError::new(
                "resolve VDS disk number",
                "VDS returned neither a disk name nor a device path",
            ))
        } else {
            Err(StorageError::new(
                "resolve VDS disk number",
                errors.join("; "),
            ))
        }
    }

    unsafe fn disk_number_from_opaque_path(
        value: PWSTR,
        label: &'static str,
    ) -> Result<Option<u32>, StorageError> {
        let path = PCWSTR(value.0.cast_const());
        let zero_access = CreateFileW(
            path,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle);
        match zero_access {
            Ok(handle) => match query_present_disk_device_number(handle.0) {
                Ok(value) => Ok(value.map(|(number, _)| number)),
                Err(zero_error) => {
                    let read = CreateFileW(
                        path,
                        GENERIC_READ.0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        None,
                        OPEN_EXISTING,
                        Default::default(),
                        None,
                    )
                    .map(OwnedHandle)
                    .map_err(|read_error| {
                        StorageError::new(
                            "resolve VDS disk number",
                            format!(
                                "{label} zero-access IOCTL failed ({zero_error}); read-only open failed ({read_error})"
                            ),
                        )
                    })?;
                    query_present_disk_device_number(read.0)
                        .map(|value| value.map(|(number, _)| number))
                        .map_err(|read_error| {
                            StorageError::new(
                                "resolve VDS disk number",
                                format!(
                                    "{label} zero-access IOCTL failed ({zero_error}); read-only IOCTL failed ({read_error})"
                                ),
                            )
                        })
                }
            },
            Err(zero_open_error) => {
                let read = CreateFileW(
                    path,
                    GENERIC_READ.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    Default::default(),
                    None,
                )
                .map(OwnedHandle)
                .map_err(|read_error| {
                    StorageError::new(
                        "resolve VDS disk number",
                        format!(
                            "{label} zero-access open failed ({zero_open_error}); read-only open failed ({read_error})"
                        ),
                    )
                })?;
                query_present_disk_device_number(read.0)
                    .map(|value| value.map(|(number, _)| number))
                    .map_err(|read_error| {
                        StorageError::new(
                            "resolve VDS disk number",
                            format!(
                                "{label} zero-access open failed ({zero_open_error}); read-only IOCTL failed ({read_error})"
                            ),
                        )
                    })
            }
        }
    }

    unsafe fn copy_pwstr(value: PWSTR) -> Option<String> {
        if value.is_null() {
            None
        } else {
            value.to_string().ok()
        }
    }

    unsafe fn free_pwstr(value: &mut PWSTR) {
        if !value.is_null() {
            co_task_mem_free(value.0.cast::<c_void>());
            *value = PWSTR::null();
        }
    }

    unsafe fn free_disk_properties(properties: &mut VDS_DISK_PROP) {
        free_pwstr(&mut properties.pwszDiskAddress);
        free_pwstr(&mut properties.pwszName);
        free_pwstr(&mut properties.pwszFriendlyName);
        free_pwstr(&mut properties.pwszAdaptorName);
        free_pwstr(&mut properties.pwszDevicePath);
    }

    fn vds_volume_device_path(name: &str) -> Result<&str, StorageError> {
        let device_path = name.trim_end_matches('\\');
        if device_path.starts_with(r"\\?\")
            && device_path.len() > 4
            && !device_path[4..].contains(['\\', '/'])
        {
            // A VDS volume name is an opaque CreateFile-compatible device path. Microsoft gives
            // `\\?\GLOBALROOT\Device\HarddiskVolume1` as an example; it is not required to be a
            // Mount Manager `\\?\Volume{GUID}` path. Reject only path traversal/separators beyond
            // the fixed Win32 device prefix, then let CreateFile + IOCTL extent readback prove the
            // actual object before any destructive caller proceeds.
            return Ok(device_path);
        }
        if device_path.starts_with(r"\\?\GLOBALROOT\Device\HarddiskVolume")
            && device_path[r"\\?\GLOBALROOT\Device\HarddiskVolume".len()..]
                .chars()
                .all(|value| value.is_ascii_digit())
        {
            Ok(device_path)
        } else {
            Err(StorageError::new(
                "validate VDS volume name",
                format!("VDS returned an unexpected volume name: {name}"),
            ))
        }
    }

    unsafe fn volume_identity_from_vds_object(
        volume: &IVdsVolume,
    ) -> Result<VolumeIdentity, StorageError> {
        let name = volume_guid_device_path_from_vds_object(volume)?;
        volume_identity_from_device_path(&name)
    }

    unsafe fn find_volume_for_exact_extent(
        vds: &Vds,
        expected: VolumeIdentity,
    ) -> Result<Option<IVdsVolume>, StorageError> {
        let mut matched = None;
        for volume in vds.volumes()? {
            // Ordinary data partitions are required by the IVdsAdvancedDisk contract to be VDS
            // volumes. Unlike the canonical ESP path, an unreadable object here makes the global
            // ordinary-volume enumeration incomplete, so it must not be silently downgraded into
            // an AdvancedDisk fallback.
            let actual = volume_identity_from_vds_object(&volume)?;
            if same_volume_identity(actual, expected) && matched.replace(volume).is_some() {
                return Err(StorageError::new(
                    "bind VDS volume by physical extent",
                    "multiple VDS volume objects report the same physical extent",
                ));
            }
        }
        Ok(matched)
    }

    unsafe fn volume_guid_device_path_from_vds_object(
        volume: &IVdsVolume,
    ) -> Result<String, StorageError> {
        let mut properties = VDS_VOLUME_PROP::default();
        let result =
            (Interface::vtable(volume).GetProperties)(Interface::as_raw(volume), &mut properties);
        let incomplete = match validate_vds_volume_name_result(result) {
            Ok(incomplete) => incomplete,
            Err(error) => {
                free_pwstr(&mut properties.pwszName);
                return Err(error);
            }
        };
        let name = copy_pwstr(properties.pwszName).ok_or_else(|| {
            StorageError::new("read VDS volume properties", "VDS volume name is missing")
        });
        free_pwstr(&mut properties.pwszName);
        let name = name?;
        if incomplete {
            // Microsoft documents VDS_S_PROPERTIES_INCOMPLETE as a successful partial property
            // read. This call site consumes only pwszName, validates it below, and then derives the
            // physical extent from the opened volume handle; unrelated missing status/health fields
            // must not block a supported VDS operation.
            log::debug!(
                "VDS returned partial volume properties; using the returned validated volume name"
            );
        }
        Ok(vds_volume_device_path(&name)?.to_owned())
    }

    fn validate_vds_volume_name_result(result: HRESULT) -> Result<bool, StorageError> {
        if result == HRESULT(0) {
            Ok(false)
        } else if result == VDS_S_PROPERTIES_INCOMPLETE_HRESULT {
            Ok(true)
        } else {
            Err(hresult_error("read VDS volume properties", result))
        }
    }

    fn validate_vds_disk_locator_result(result: HRESULT) -> Result<bool, StorageError> {
        if result == HRESULT(0) {
            Ok(false)
        } else if result == VDS_S_PROPERTIES_INCOMPLETE_HRESULT {
            // Only the locator is consumed. The opened handle supplies the authoritative disk
            // number, so unrelated missing VDS health/status properties cannot invalidate it.
            Ok(true)
        } else {
            Err(hresult_error("read VDS disk locator", result))
        }
    }

    unsafe fn volume_guid_device_path_from_drive_letter(
        drive_letter: char,
    ) -> Result<String, StorageError> {
        let letter = normalize_letter(drive_letter)?;
        let root = wide(&format!("{letter}:\\"));
        let mut buffer = [0_u16; 128];
        GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut buffer)
            .map_err(|error| api_error("resolve drive-letter volume GUID", error))?;
        let name = volume_name_from_buffer(&buffer)?;
        Ok(vds_volume_device_path(&name)?.to_owned())
    }

    fn normalize_letter(letter: char) -> Result<char, StorageError> {
        let letter = letter.to_ascii_uppercase();
        if ('C'..='Z').contains(&letter) {
            Ok(letter)
        } else {
            Err(StorageError::new(
                "validate drive letter",
                "drive letter must be in the C-Z range",
            ))
        }
    }

    pub fn current_windows_drive_letter() -> Result<char, StorageError> {
        let mut buffer = [0_u16; 32_768];
        let length = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(StorageError::new(
                "locate running Windows volume",
                "GetWindowsDirectoryW returned an invalid path length",
            ));
        }
        let path = String::from_utf16(&buffer[..length]).map_err(|error| {
            StorageError::new(
                "locate running Windows volume",
                format!("GetWindowsDirectoryW returned invalid UTF-16: {error}"),
            )
        })?;
        let mut characters = path.chars();
        let letter = characters.next().ok_or_else(|| {
            StorageError::new(
                "locate running Windows volume",
                "Windows directory path is empty",
            )
        })?;
        if characters.next() != Some(':') {
            return Err(StorageError::new(
                "locate running Windows volume",
                format!("Windows directory is not drive-letter based: {path}"),
            ));
        }
        normalize_letter(letter)
    }

    pub fn assigned_drive_letter_mask() -> Result<u32, StorageError> {
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Err(StorageError::new(
                "enumerate assigned drive letters",
                windows::core::Error::from_win32().to_string(),
            ));
        }
        Ok(mask)
    }

    fn is_variable_buffer_error(error: &windows::core::Error) -> bool {
        error.code() == HRESULT::from_win32(ERROR_MORE_DATA.0)
            || error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
    }

    unsafe fn volume_extents_from_device_path(
        device_path: &str,
    ) -> Result<Vec<windows::Win32::System::Ioctl::DISK_EXTENT>, StorageError> {
        use windows::Win32::Storage::FileSystem::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS;
        use windows::Win32::System::Ioctl::{DISK_EXTENT, VOLUME_DISK_EXTENTS};
        use windows::Win32::System::IO::DeviceIoControl;

        let path = wide(device_path);
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| api_error("open volume identity", error))?;
        let mut capacity = size_of::<VOLUME_DISK_EXTENTS>().max(256);
        loop {
            if capacity > 1024 * 1024 {
                return Err(StorageError::new(
                    "query volume disk extents",
                    "volume extent response exceeds the one-megabyte safety limit",
                ));
            }
            let word_count = capacity.div_ceil(size_of::<u64>());
            let mut storage = vec![0_u64; word_count];
            let mut returned = 0_u32;
            match DeviceIoControl(
                handle.0,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                None,
                0,
                Some(storage.as_mut_ptr().cast()),
                (storage.len() * size_of::<u64>()) as u32,
                Some(&mut returned),
                None,
            ) {
                Ok(()) => {
                    if returned < size_of::<VOLUME_DISK_EXTENTS>() as u32 {
                        return Err(StorageError::new(
                            "query volume disk extents",
                            "response is shorter than VOLUME_DISK_EXTENTS",
                        ));
                    }
                    let header =
                        std::ptr::read_unaligned(storage.as_ptr().cast::<VOLUME_DISK_EXTENTS>());
                    let count = usize::try_from(header.NumberOfDiskExtents).map_err(|_| {
                        StorageError::new(
                            "query volume disk extents",
                            "extent count does not fit in memory",
                        )
                    })?;
                    if count == 0 {
                        return Err(StorageError::new(
                            "query volume disk extents",
                            "volume reports zero disk extents",
                        ));
                    }
                    let first_offset = std::mem::offset_of!(VOLUME_DISK_EXTENTS, Extents);
                    let required = first_offset
                        .checked_add(count.checked_mul(size_of::<DISK_EXTENT>()).ok_or_else(
                            || {
                                StorageError::new(
                                    "query volume disk extents",
                                    "extent response size overflow",
                                )
                            },
                        )?)
                        .ok_or_else(|| {
                            StorageError::new(
                                "query volume disk extents",
                                "extent response size overflow",
                            )
                        })?;
                    if required > returned as usize || required > storage.len() * size_of::<u64>() {
                        return Err(StorageError::new(
                            "query volume disk extents",
                            "extent response is shorter than its declared count",
                        ));
                    }
                    let first = storage.as_ptr().cast::<u8>().add(first_offset);
                    return Ok((0..count)
                        .map(|index| {
                            std::ptr::read_unaligned(
                                first
                                    .add(index * size_of::<DISK_EXTENT>())
                                    .cast::<DISK_EXTENT>(),
                            )
                        })
                        .collect());
                }
                Err(error) if is_variable_buffer_error(&error) => {
                    capacity = capacity.checked_mul(2).ok_or_else(|| {
                        StorageError::new("query volume disk extents", "buffer size overflow")
                    })?;
                }
                Err(error) => return Err(api_error("query volume disk extents", error)),
            }
        }
    }

    fn same_volume_extent_set(
        left: &[windows::Win32::System::Ioctl::DISK_EXTENT],
        right: &[windows::Win32::System::Ioctl::DISK_EXTENT],
    ) -> bool {
        let normalized = |values: &[windows::Win32::System::Ioctl::DISK_EXTENT]| {
            let mut values = values
                .iter()
                .map(|value| (value.DiskNumber, value.StartingOffset, value.ExtentLength))
                .collect::<Vec<_>>();
            values.sort_unstable();
            values
        };
        normalized(left) == normalized(right)
    }

    unsafe fn volume_identity_from_device_path(
        device_path: &str,
    ) -> Result<VolumeIdentity, StorageError> {
        let extents = volume_extents_from_device_path(device_path)?;
        if extents.len() != 1 {
            return Err(StorageError::new(
                "query volume disk extents",
                format!("expected one basic-disk extent, received {}", extents.len()),
            ));
        }
        let extent = extents[0];
        let offset_bytes = u64::try_from(extent.StartingOffset).map_err(|_| {
            StorageError::new("query volume disk extents", "volume offset is negative")
        })?;
        let extent_length_bytes = u64::try_from(extent.ExtentLength).map_err(|_| {
            StorageError::new(
                "query volume disk extents",
                "volume extent length is negative",
            )
        })?;
        if extent_length_bytes == 0 {
            return Err(StorageError::new(
                "query volume disk extents",
                "volume reports an empty physical extent",
            ));
        }
        Ok(VolumeIdentity {
            disk_number: extent.DiskNumber,
            offset_bytes,
            extent_length_bytes,
        })
    }

    pub unsafe fn volume_identity(drive_letter: char) -> Result<VolumeIdentity, StorageError> {
        let letter = normalize_letter(drive_letter)?;
        volume_identity_from_device_path(&format!(r"\\.\{letter}:"))
    }

    fn guid_identity(guid: GUID) -> [u8; 16] {
        guid.to_u128().to_le_bytes()
    }

    unsafe fn normalized_device_identifiers(
        bytes: &[u8],
    ) -> Result<Vec<(i32, i32, Vec<u8>)>, StorageError> {
        use windows::Win32::System::Ioctl::{
            StorageIdAssocDevice, STORAGE_DEVICE_ID_DESCRIPTOR, STORAGE_IDENTIFIER,
        };

        if bytes.len() < size_of::<STORAGE_DEVICE_ID_DESCRIPTOR>() {
            return Err(StorageError::new(
                "query physical disk device identity",
                "storage device-id descriptor was truncated",
            ));
        }
        let descriptor =
            std::ptr::read_unaligned(bytes.as_ptr().cast::<STORAGE_DEVICE_ID_DESCRIPTOR>());
        let declared_size = descriptor.Size as usize;
        if declared_size < size_of::<STORAGE_DEVICE_ID_DESCRIPTOR>() || declared_size > bytes.len()
        {
            return Err(StorageError::new(
                "query physical disk device identity",
                "storage device-id descriptor declared an invalid size",
            ));
        }
        let bytes = &bytes[..declared_size];
        let mut identifiers = Vec::new();
        let mut offset = std::mem::offset_of!(STORAGE_DEVICE_ID_DESCRIPTOR, Identifiers);
        for index in 0..descriptor.NumberOfIdentifiers {
            let fixed = std::mem::offset_of!(STORAGE_IDENTIFIER, Identifier);
            if offset
                .checked_add(size_of::<STORAGE_IDENTIFIER>())
                .is_none_or(|end| end > bytes.len())
            {
                return Err(StorageError::new(
                    "query physical disk device identity",
                    "storage identifier fixed record exceeds the descriptor",
                ));
            }
            let identifier =
                std::ptr::read_unaligned(bytes.as_ptr().add(offset).cast::<STORAGE_IDENTIFIER>());
            let value_start = offset + fixed;
            let value_end = value_start
                .checked_add(usize::from(identifier.IdentifierSize))
                .ok_or_else(|| {
                    StorageError::new(
                        "query physical disk device identity",
                        "storage identifier size overflow",
                    )
                })?;
            if value_end > bytes.len() || identifier.IdentifierSize == 0 {
                return Err(StorageError::new(
                    "query physical disk device identity",
                    "storage identifier value is empty or truncated",
                ));
            }
            if identifier.Association == StorageIdAssocDevice {
                identifiers.push((
                    identifier.Type.0,
                    identifier.CodeSet.0,
                    bytes[value_start..value_end].to_vec(),
                ));
            }
            if index + 1 < descriptor.NumberOfIdentifiers {
                let next = usize::from(identifier.NextOffset);
                if next < fixed + usize::from(identifier.IdentifierSize)
                    || offset
                        .checked_add(next)
                        .is_none_or(|next| next >= bytes.len())
                {
                    return Err(StorageError::new(
                        "query physical disk device identity",
                        "storage identifier next offset is invalid",
                    ));
                }
                offset += next;
            }
        }
        identifiers.sort();
        identifiers.dedup();
        Ok(identifiers)
    }

    unsafe fn physical_device_id_hash_from_handle(
        handle: HANDLE,
    ) -> Result<Option<[u8; 32]>, StorageError> {
        use sha2::{Digest, Sha256};
        use windows::Win32::System::Ioctl::{
            PropertyExistsQuery, PropertyStandardQuery, StorageDeviceIdProperty,
            IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DESCRIPTOR_HEADER, STORAGE_DEVICE_ID_DESCRIPTOR,
            STORAGE_PROPERTY_QUERY,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
        let exists_query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceIdProperty,
            QueryType: PropertyExistsQuery,
            AdditionalParameters: [0],
        };
        let mut returned = 0u32;
        if let Err(error) = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&exists_query as *const STORAGE_PROPERTY_QUERY).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        ) {
            if storage_device_id_property_unavailable(error.code()) {
                log::debug!(
                    "physical disk does not expose optional StorageDeviceIdProperty ({error})"
                );
                return Ok(None);
            }
            return Err(api_error(
                "check physical disk device identity availability",
                error,
            ));
        }
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceIdProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut header = STORAGE_DESCRIPTOR_HEADER::default();
        returned = 0;
        if let Err(error) = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some((&mut header as *mut STORAGE_DESCRIPTOR_HEADER).cast()),
            size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32,
            Some(&mut returned),
            None,
        ) {
            if storage_device_id_property_unavailable(error.code()) {
                log::debug!(
                    "physical disk rejected optional StorageDeviceIdProperty descriptor query ({error})"
                );
                return Ok(None);
            }
            return Err(api_error(
                "query physical disk device identity descriptor size",
                error,
            ));
        }
        if returned < size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32 {
            return Err(StorageError::new(
                "query physical disk device identity",
                "storage device-id descriptor header was truncated",
            ));
        }
        let descriptor_size = usize::try_from(header.Size).map_err(|_| {
            StorageError::new(
                "query physical disk device identity",
                "storage device-id descriptor size does not fit in memory",
            )
        })?;
        if descriptor_size < size_of::<STORAGE_DEVICE_ID_DESCRIPTOR>()
            || descriptor_size > MAX_DESCRIPTOR_BYTES
        {
            return Err(StorageError::new(
                "query physical disk device identity",
                format!("invalid storage device-id descriptor size: {descriptor_size}"),
            ));
        }
        let mut storage = vec![0u64; descriptor_size.div_ceil(size_of::<u64>())];
        returned = 0;
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(storage.as_mut_ptr().cast()),
            (storage.len() * size_of::<u64>()) as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query physical disk device identity", error))?;
        let available = usize::min(returned as usize, descriptor_size);
        let bytes = std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), available);
        let identifiers = normalized_device_identifiers(bytes)?;
        if identifiers.is_empty() {
            return Ok(None);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"LetRecovery.StorageDeviceId.V1\0");
        for (identifier_type, code_set, value) in identifiers {
            hasher.update(identifier_type.to_le_bytes());
            hasher.update(code_set.to_le_bytes());
            hasher.update((value.len() as u32).to_le_bytes());
            hasher.update(value);
        }
        Ok(Some(hasher.finalize().into()))
    }

    unsafe fn stable_identity_for_extent(
        extent: VolumeIdentity,
    ) -> Result<StableVolumeIdentity, StorageError> {
        use windows::Win32::System::Ioctl::{
            DRIVE_LAYOUT_INFORMATION_EX, PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT,
            PARTITION_STYLE_MBR,
        };

        let (_, storage, returned) = read_drive_layout(extent.disk_number, false)?;
        let layout_offset = std::mem::offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        if (returned as usize) < layout_offset {
            return Err(StorageError::new(
                "query stable volume identity",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout =
            std::ptr::read_unaligned(storage.as_ptr().cast::<DRIVE_LAYOUT_INFORMATION_EX>());
        let count = usize::try_from(layout.PartitionCount).map_err(|_| {
            StorageError::new(
                "query stable volume identity",
                "partition count does not fit in memory",
            )
        })?;
        let required = layout_offset
            .checked_add(
                count
                    .checked_mul(size_of::<PARTITION_INFORMATION_EX>())
                    .ok_or_else(|| {
                        StorageError::new(
                            "query stable volume identity",
                            "partition layout size overflow",
                        )
                    })?,
            )
            .ok_or_else(|| {
                StorageError::new(
                    "query stable volume identity",
                    "partition layout size overflow",
                )
            })?;
        if required > returned as usize {
            return Err(StorageError::new(
                "query stable volume identity",
                "drive layout response is shorter than its declared partition count",
            ));
        }
        let first = storage.as_ptr().cast::<u8>().add(layout_offset);
        let mut matched = None;
        for index in 0..count {
            let partition = std::ptr::read_unaligned(
                first
                    .add(index * size_of::<PARTITION_INFORMATION_EX>())
                    .cast::<PARTITION_INFORMATION_EX>(),
            );
            if partition.StartingOffset < 0 || partition.PartitionLength <= 0 {
                continue;
            }
            if partition.StartingOffset as u64 == extent.offset_bytes
                && partition.PartitionLength as u64 == extent.extent_length_bytes
                && matched.replace(partition).is_some()
            {
                return Err(StorageError::new(
                    "query stable volume identity",
                    "multiple partition records match the same volume extent",
                ));
            }
        }
        let partition = matched.ok_or_else(|| {
            StorageError::new(
                "query stable volume identity",
                "no exact partition record matches the volume extent",
            )
        })?;
        let (disk, partition) = if layout.PartitionStyle == PARTITION_STYLE_GPT.0 as u32
            && partition.PartitionStyle == PARTITION_STYLE_GPT
        {
            let disk_id = guid_identity(layout.Anonymous.Gpt.DiskId);
            let partition_id = guid_identity(partition.Anonymous.Gpt.PartitionId);
            if disk_id == [0; 16] || partition_id == [0; 16] {
                return Err(StorageError::new(
                    "query stable volume identity",
                    "GPT disk or partition identifier is zero",
                ));
            }
            (
                StableDiskIdentity::Gpt { disk_id },
                StablePartitionIdentity::Gpt { partition_id },
            )
        } else if layout.PartitionStyle == PARTITION_STYLE_MBR.0 as u32
            && partition.PartitionStyle == PARTITION_STYLE_MBR
        {
            let signature = layout.Anonymous.Mbr.Signature;
            if signature == 0 || partition.PartitionNumber == 0 {
                return Err(StorageError::new(
                    "query stable volume identity",
                    "MBR disk signature or partition number is zero",
                ));
            }
            (
                StableDiskIdentity::Mbr { signature },
                StablePartitionIdentity::Mbr {
                    partition_number: partition.PartitionNumber,
                },
            )
        } else {
            return Err(StorageError::new(
                "query stable volume identity",
                "volume is not backed by a consistently identified GPT or MBR partition",
            ));
        };
        let device_id_hash = disk_layout_snapshot(extent.disk_number)?.device_id_hash;
        Ok(StableVolumeIdentity {
            extent,
            disk,
            partition,
            device_id_hash,
        })
    }

    unsafe fn canonical_partition_number_for_extent(
        extent: VolumeIdentity,
    ) -> Result<u32, StorageError> {
        use windows::Win32::System::Ioctl::{
            DRIVE_LAYOUT_INFORMATION_EX, PARTITION_INFORMATION_EX,
        };

        let (_, storage, returned) = read_drive_layout(extent.disk_number, false)?;
        let layout_offset = std::mem::offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        if (returned as usize) < layout_offset {
            return Err(StorageError::new(
                "bind current partition number",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout =
            std::ptr::read_unaligned(storage.as_ptr().cast::<DRIVE_LAYOUT_INFORMATION_EX>());
        let count = usize::try_from(layout.PartitionCount).map_err(|_| {
            StorageError::new(
                "bind current partition number",
                "partition count does not fit in memory",
            )
        })?;
        let required = layout_offset
            .checked_add(
                count
                    .checked_mul(size_of::<PARTITION_INFORMATION_EX>())
                    .ok_or_else(|| {
                        StorageError::new(
                            "bind current partition number",
                            "partition layout size overflow",
                        )
                    })?,
            )
            .ok_or_else(|| {
                StorageError::new(
                    "bind current partition number",
                    "partition layout size overflow",
                )
            })?;
        if required > returned as usize {
            return Err(StorageError::new(
                "bind current partition number",
                "drive layout response is shorter than its declared partition count",
            ));
        }
        let first = storage.as_ptr().cast::<u8>().add(layout_offset);
        let mut matched = None;
        for index in 0..count {
            let partition = std::ptr::read_unaligned(
                first
                    .add(index * size_of::<PARTITION_INFORMATION_EX>())
                    .cast::<PARTITION_INFORMATION_EX>(),
            );
            if partition.StartingOffset < 0 || partition.PartitionLength <= 0 {
                continue;
            }
            if partition.StartingOffset as u64 == extent.offset_bytes
                && partition.PartitionLength as u64 == extent.extent_length_bytes
                && (partition.PartitionNumber == 0
                    || matched.replace(partition.PartitionNumber).is_some())
            {
                return Err(StorageError::new(
                    "bind current partition number",
                    "UntrustedStorage: exact extent has a zero or duplicate current partition number",
                ));
            }
        }
        matched.ok_or_else(|| {
            StorageError::new(
                "bind current partition number",
                "UntrustedStorage: no current partition number maps to the exact authorized extent",
            )
        })
    }

    unsafe fn disk_layout_snapshot_from_handle(
        handle: HANDLE,
    ) -> Result<DiskLayoutSnapshot, StorageError> {
        use windows::Win32::System::Ioctl::{
            DRIVE_LAYOUT_INFORMATION_EX, PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT,
            PARTITION_STYLE_MBR,
        };

        let (storage, returned) = read_drive_layout_from_raw_handle(handle)?;
        let layout_offset = std::mem::offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        if (returned as usize) < layout_offset {
            return Err(StorageError::new(
                "snapshot physical disk layout",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout =
            std::ptr::read_unaligned(storage.as_ptr().cast::<DRIVE_LAYOUT_INFORMATION_EX>());
        let count = usize::try_from(layout.PartitionCount).map_err(|_| {
            StorageError::new(
                "snapshot physical disk layout",
                "partition count does not fit in memory",
            )
        })?;
        let required = layout_offset
            .checked_add(
                count
                    .checked_mul(size_of::<PARTITION_INFORMATION_EX>())
                    .ok_or_else(|| {
                        StorageError::new(
                            "snapshot physical disk layout",
                            "partition layout size overflow",
                        )
                    })?,
            )
            .ok_or_else(|| {
                StorageError::new(
                    "snapshot physical disk layout",
                    "partition layout size overflow",
                )
            })?;
        if required > returned as usize {
            return Err(StorageError::new(
                "snapshot physical disk layout",
                "drive layout response is shorter than its declared partition count",
            ));
        }
        let device_id_hash = physical_device_id_hash_from_handle(handle)?;
        let disk = if layout.PartitionStyle == PARTITION_STYLE_GPT.0 as u32 {
            let disk_id = guid_identity(layout.Anonymous.Gpt.DiskId);
            if disk_id == [0; 16] {
                return Err(StorageError::new(
                    "snapshot physical disk layout",
                    "GPT disk identifier is zero",
                ));
            }
            StableDiskIdentity::Gpt { disk_id }
        } else if layout.PartitionStyle == PARTITION_STYLE_MBR.0 as u32 {
            let signature = layout.Anonymous.Mbr.Signature;
            if signature == 0 {
                return Err(StorageError::new(
                    "snapshot physical disk layout",
                    "MBR disk signature is zero",
                ));
            }
            StableDiskIdentity::Mbr { signature }
        } else if layout.PartitionStyle
            == windows::Win32::System::Ioctl::PARTITION_STYLE_RAW.0 as u32
            && count == 0
        {
            if device_id_hash.is_none() {
                return Err(StorageError::new(
                    "snapshot physical disk layout",
                    "RAW disk exposes no device-level storage identifier",
                ));
            }
            StableDiskIdentity::Raw
        } else {
            return Err(StorageError::new(
                "snapshot physical disk layout",
                "disk is not initialized as GPT or MBR",
            ));
        };
        let first = storage.as_ptr().cast::<u8>().add(layout_offset);
        let mut partitions = Vec::with_capacity(count);
        for index in 0..count {
            let partition = std::ptr::read_unaligned(
                first
                    .add(index * size_of::<PARTITION_INFORMATION_EX>())
                    .cast::<PARTITION_INFORMATION_EX>(),
            );
            if partition.StartingOffset < 0 || partition.PartitionLength <= 0 {
                continue;
            }
            let token = match disk {
                StableDiskIdentity::Gpt { .. }
                    if partition.PartitionStyle == PARTITION_STYLE_GPT =>
                {
                    let metadata = partition.Anonymous.Gpt;
                    let partition_type = guid_identity(metadata.PartitionType);
                    let partition_id = guid_identity(metadata.PartitionId);
                    if partition_type == [0; 16] || partition_id == [0; 16] {
                        return Err(StorageError::new(
                            "snapshot physical disk layout",
                            "GPT partition type or identifier is zero",
                        ));
                    }
                    DiskLayoutPartitionToken::Gpt {
                        partition_type,
                        partition_id,
                        attributes: metadata.Attributes.0,
                    }
                }
                StableDiskIdentity::Mbr { .. }
                    if partition.PartitionStyle == PARTITION_STYLE_MBR =>
                {
                    let metadata = partition.Anonymous.Mbr;
                    DiskLayoutPartitionToken::Mbr {
                        partition_type: metadata.PartitionType,
                        boot_indicator: metadata.BootIndicator.0 != 0,
                    }
                }
                StableDiskIdentity::Raw => {
                    return Err(StorageError::new(
                        "snapshot physical disk layout",
                        "RAW disk unexpectedly reports a partition record",
                    ));
                }
                _ => {
                    return Err(StorageError::new(
                        "snapshot physical disk layout",
                        "partition style does not match its disk layout style",
                    ));
                }
            };
            partitions.push(DiskLayoutPartitionSnapshot {
                offset_bytes: partition.StartingOffset as u64,
                size_bytes: partition.PartitionLength as u64,
                token,
            });
        }
        partitions.sort();
        if partitions.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(StorageError::new(
                "snapshot physical disk layout",
                "drive layout contains duplicate partition records",
            ));
        }

        // The caller's handle must carry GENERIC_READ because this IOCTL carries
        // FILE_READ_ACCESS. Reusing the same handle binds layout, length and device ID to one
        // opened PhysicalDrive object instead of mixing observations across reopen races.
        let disk_size_bytes = disk_length_from_raw_handle(handle)?;
        if disk_size_bytes == 0 {
            return Err(StorageError::new(
                "snapshot physical disk layout",
                "physical disk reports zero capacity",
            ));
        }
        Ok(DiskLayoutSnapshot {
            disk_size_bytes,
            disk,
            device_id_hash,
            partitions,
        })
    }

    unsafe fn gpt_partition_metadata_at(
        disk_number: u32,
        created: CreatedPartition,
    ) -> Result<GptPartitionMetadata, StorageError> {
        use windows::Win32::System::Ioctl::{
            DRIVE_LAYOUT_INFORMATION_EX, PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT,
        };

        let (_, storage, returned) = read_drive_layout(disk_number, false)?;
        let layout_offset = std::mem::offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        if (returned as usize) < layout_offset {
            return Err(StorageError::new(
                "verify preserved GPT metadata",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout =
            std::ptr::read_unaligned(storage.as_ptr().cast::<DRIVE_LAYOUT_INFORMATION_EX>());
        if layout.PartitionStyle != PARTITION_STYLE_GPT.0 as u32 {
            return Err(StorageError::new(
                "verify preserved GPT metadata",
                "target disk is no longer GPT",
            ));
        }
        let count = usize::try_from(layout.PartitionCount).map_err(|_| {
            StorageError::new(
                "verify preserved GPT metadata",
                "partition count does not fit in memory",
            )
        })?;
        let required = layout_offset
            .checked_add(
                count
                    .checked_mul(size_of::<PARTITION_INFORMATION_EX>())
                    .ok_or_else(|| {
                        StorageError::new(
                            "verify preserved GPT metadata",
                            "partition layout size overflow",
                        )
                    })?,
            )
            .ok_or_else(|| {
                StorageError::new(
                    "verify preserved GPT metadata",
                    "partition layout size overflow",
                )
            })?;
        if required > returned as usize {
            return Err(StorageError::new(
                "verify preserved GPT metadata",
                "drive layout response is shorter than its declared partition count",
            ));
        }

        let first = storage.as_ptr().cast::<u8>().add(layout_offset);
        let mut found = None;
        for index in 0..count {
            let partition = std::ptr::read_unaligned(
                first
                    .add(index * size_of::<PARTITION_INFORMATION_EX>())
                    .cast::<PARTITION_INFORMATION_EX>(),
            );
            if partition.StartingOffset < 0 || partition.PartitionLength <= 0 {
                continue;
            }
            if partition.StartingOffset as u64 != created.offset_bytes
                || partition.PartitionLength as u64 != created.size_bytes
            {
                continue;
            }
            if partition.PartitionStyle != PARTITION_STYLE_GPT {
                return Err(StorageError::new(
                    "verify preserved GPT metadata",
                    "the recreated extent is not a GPT partition",
                ));
            }
            let metadata = partition.Anonymous.Gpt;
            let candidate = GptPartitionMetadata {
                partition_id: guid_identity(metadata.PartitionId),
                attributes: metadata.Attributes.0,
                name: metadata.Name,
            };
            if found.replace(candidate).is_some() {
                return Err(StorageError::new(
                    "verify preserved GPT metadata",
                    "multiple GPT records describe the recreated extent",
                ));
            }
        }
        found.ok_or_else(|| {
            StorageError::new(
                "verify preserved GPT metadata",
                "the recreated GPT extent is missing from the current layout",
            )
        })
    }

    pub unsafe fn disk_layout_snapshot(
        disk_number: u32,
    ) -> Result<DiskLayoutSnapshot, StorageError> {
        trusted_present_disk_snapshot(disk_number).map(|(_, snapshot)| snapshot)
    }

    pub unsafe fn physical_disk_numbers() -> Result<Vec<u32>, StorageError> {
        present_physical_disk_numbers()
    }

    pub unsafe fn verify_disk_layout_snapshot_from_handle(
        handle: HANDLE,
        expected: &DiskLayoutSnapshot,
    ) -> Result<(), StorageError> {
        let actual = disk_layout_snapshot_from_handle(handle)?;
        if &actual != expected {
            return Err(StorageError::new(
                "verify opened physical disk snapshot",
                "opened physical disk identity or canonical partition layout changed",
            ));
        }
        Ok(())
    }

    pub unsafe fn stable_volume_identity(
        drive_letter: char,
    ) -> Result<StableVolumeIdentity, StorageError> {
        let mounted_extent = volume_identity(drive_letter)?;
        let volume_guid_path = volume_guid_device_path_from_drive_letter(drive_letter)?;
        let volume_guid_extent = volume_identity_from_device_path(&volume_guid_path)?;
        let disk_snapshot = disk_layout_snapshot(mounted_extent.disk_number)?;
        verify_current_volume_identity_closure(mounted_extent, volume_guid_extent, &disk_snapshot)?;
        stable_identity_for_extent(mounted_extent)
    }

    pub unsafe fn stable_volume_identity_from_guid_path(
        volume_guid_root: &str,
    ) -> Result<StableVolumeIdentity, StorageError> {
        let extent = volume_identity_from_guid_path(volume_guid_root)?;
        let disk_snapshot = disk_layout_snapshot(extent.disk_number)?;
        verify_current_volume_identity_closure(extent, extent, &disk_snapshot)?;
        stable_identity_for_extent(extent)
    }

    pub fn drive_kind(drive_letter: char) -> Result<DriveKind, StorageError> {
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_FIXED: u32 = 3;
        const DRIVE_REMOTE: u32 = 4;
        const DRIVE_CDROM: u32 = 5;
        const DRIVE_RAMDISK: u32 = 6;
        let letter = normalize_letter(drive_letter)?;
        let root = wide(&format!("{letter}:\\"));
        match unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } {
            DRIVE_REMOVABLE => Ok(DriveKind::Removable),
            DRIVE_FIXED => Ok(DriveKind::Fixed),
            DRIVE_REMOTE => Ok(DriveKind::Remote),
            DRIVE_CDROM => Ok(DriveKind::Optical),
            DRIVE_RAMDISK => Ok(DriveKind::RamDisk),
            value => Err(StorageError::new(
                "classify drive",
                format!("GetDriveTypeW returned unsupported drive type {value} for {letter}:"),
            )),
        }
    }

    /// `StorageDeviceIdProperty` is VPD page 0x83 evidence and is optional for many legitimate
    /// storage stacks. Microsoft documents INVALID_DEVICE_REQUEST, INVALID_PARAMETER and
    /// NOT_SUPPORTED as possible `IOCTL_STORAGE_QUERY_PROPERTY` statuses. Win32 exposes the first
    /// as ERROR_INVALID_FUNCTION. Treat only those three results as property unavailability; all
    /// other failures still stop the snapshot. GPT/MBR disks remain bound by the same handle's
    /// capacity, partition-table token and exact extents, while RAW disks continue to require a
    /// device-level identifier before destructive use.
    pub(super) fn storage_device_id_property_unavailable(code: HRESULT) -> bool {
        code == HRESULT::from_win32(ERROR_INVALID_FUNCTION.0)
            || code == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0)
            || code == HRESULT::from_win32(ERROR_NOT_SUPPORTED.0)
    }

    fn volume_name_from_buffer(buffer: &[u16]) -> Result<String, StorageError> {
        let end = buffer.iter().position(|value| *value == 0).ok_or_else(|| {
            StorageError::new(
                "enumerate volume GUID paths",
                "FindFirstVolumeW/FindNextVolumeW returned an unterminated path",
            )
        })?;
        let volume_name = String::from_utf16(&buffer[..end]).map_err(|error| {
            StorageError::new(
                "enumerate volume GUID paths",
                format!("volume GUID path is not valid UTF-16: {error}"),
            )
        })?;
        if !volume_name.starts_with(r"\\?\Volume{") || !volume_name.ends_with(r"}\") {
            return Err(StorageError::new(
                "enumerate volume GUID paths",
                format!("unexpected volume path returned by Windows: {volume_name}"),
            ));
        }
        Ok(volume_name)
    }

    /// Enumerate every volume currently exposed through the Windows volume namespace.
    ///
    /// The returned volume GUID roots are current-session access paths, not persistent identity
    /// fields. Microsoft explicitly documents that enumeration order has no relationship to disk
    /// order or assigned drive letters, so callers must inspect each volume independently.
    pub unsafe fn volume_guid_paths() -> Result<Vec<String>, StorageError> {
        let mut buffer = vec![0_u16; 1_024];
        let search = FindFirstVolumeW(&mut buffer)
            .map(|handle| VolumeSearchHandle(Some(handle)))
            .map_err(|error| api_error("begin volume GUID enumeration", error))?;
        let mut volumes = Vec::new();

        loop {
            volumes.push(volume_name_from_buffer(&buffer)?);
            buffer.fill(0);
            match FindNextVolumeW(search.raw(), &mut buffer) {
                Ok(()) => {}
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => break,
                Err(error) => return Err(api_error("continue volume GUID enumeration", error)),
            }
        }

        // The complete candidate set is already authoritative after FindNextVolumeW returns
        // ERROR_NO_MORE_FILES. FindVolumeClose only releases the search handle; a close failure
        // cannot invalidate the volume names already returned and must not become a locator gate.
        // RAII still attempts the documented close on every success and error path.
        Ok(volumes)
    }

    pub unsafe fn volume_identity_from_guid_path(
        volume_guid_root: &str,
    ) -> Result<VolumeIdentity, StorageError> {
        if !volume_guid_root.starts_with(r"\\?\Volume{") || !volume_guid_root.ends_with(r"}\") {
            return Err(StorageError::new(
                "resolve volume GUID extent",
                "volume GUID root has an invalid form",
            ));
        }
        volume_identity_from_device_path(volume_guid_root.trim_end_matches('\\'))
    }

    /// Resolve an exact physical partition to its existing volume GUID root without assigning a
    /// drive letter. Microsoft documents volume GUID paths as directly usable absolute roots; the
    /// trailing slash is removed only while opening the volume for the extent identity IOCTL.
    pub unsafe fn try_volume_guid_path_for_partition(
        disk_number: u32,
        offset_bytes: u64,
    ) -> Result<Option<String>, StorageError> {
        let expected = VolumeIdentity {
            disk_number,
            offset_bytes,
            extent_length_bytes: 0,
        };
        for volume_name in volume_guid_paths()? {
            let device_path = volume_name.trim_end_matches('\\');
            if let Ok(actual) = volume_identity_from_device_path(device_path) {
                if same_physical_partition(actual, expected) {
                    return Ok(Some(volume_name));
                }
            }
        }

        Ok(None)
    }

    pub unsafe fn volume_guid_path_for_partition(
        disk_number: u32,
        offset_bytes: u64,
    ) -> Result<String, StorageError> {
        try_volume_guid_path_for_partition(disk_number, offset_bytes)?.ok_or_else(|| {
            StorageError::new(
                "resolve partition volume GUID path",
                format!("no volume maps to disk {disk_number} offset {offset_bytes}"),
            )
        })
    }

    pub unsafe fn mbr_signature(disk_number: u32) -> Result<Option<u32>, StorageError> {
        let (_, storage, returned) = read_drive_layout(disk_number, false)?;
        if returned < 48 {
            return Err(StorageError::new(
                "read MBR signature",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout = &*storage
            .as_ptr()
            .cast::<windows::Win32::System::Ioctl::DRIVE_LAYOUT_INFORMATION_EX>();
        use windows::Win32::System::Ioctl::PARTITION_STYLE_MBR;
        if layout.PartitionStyle != PARTITION_STYLE_MBR.0 as u32 {
            return Ok(None);
        }
        Ok(Some(layout.Anonymous.Mbr.Signature))
    }

    pub unsafe fn disk_style(disk_number: u32) -> Result<DiskStyle, StorageError> {
        // DRIVE_LAYOUT_INFORMATION_EX::PartitionStyle is the documented result of
        // IOCTL_DISK_GET_DRIVE_LAYOUT_EX (Windows XP+). Do not use a separately enumerated VDS
        // object's cached VDS_DISK_PROP here: WinPE providers can lag the physical-disk layout and
        // once caused an actual GPT disk to enter the destructive Legacy/active-partition path.
        let (_, storage, returned) = read_drive_layout(disk_number, false)?;
        if returned < size_of::<u32>() as u32 {
            return Err(StorageError::new(
                "query disk style",
                "drive layout response does not contain PartitionStyle",
            ));
        }
        let raw = std::ptr::read_unaligned(storage.as_ptr().cast::<u32>());
        disk_style_from_layout_value(raw)
    }

    fn disk_style_from_layout_value(raw: u32) -> Result<DiskStyle, StorageError> {
        use windows::Win32::System::Ioctl::{
            PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, PARTITION_STYLE_RAW,
        };
        if raw == PARTITION_STYLE_MBR.0 as u32 {
            Ok(DiskStyle::Mbr)
        } else if raw == PARTITION_STYLE_GPT.0 as u32 {
            Ok(DiskStyle::Gpt)
        } else if raw == PARTITION_STYLE_RAW.0 as u32 {
            Err(StorageError::new(
                "query disk style",
                "disk is RAW and has no MBR or GPT partition table",
            ))
        } else {
            Err(StorageError::new(
                "query disk style",
                format!("drive layout returned unsupported partition style {raw}"),
            ))
        }
    }

    /// Read only the capacity field from the current VDS disk object for `disk_number`.
    ///
    /// This is the documented fallback when the exact SetupAPI interface rejects both capacity
    /// IOCTLs. It must never be used to synthesize a partition layout or replace the mandatory
    /// layout read from that exact SetupAPI path.
    pub unsafe fn vds_disk_size(disk_number: u32) -> Result<u64, StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        if disk.size_bytes == 0 {
            return Err(StorageError::new(
                "query VDS disk capacity",
                "VDS returned a zero disk capacity",
            ));
        }
        Ok(disk.size_bytes)
    }

    pub unsafe fn partitions(disk_number: u32) -> Result<Vec<PartitionRecord>, StorageError> {
        use windows::Win32::Storage::VirtualDiskService::VDS_PARTITION_PROP;

        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        let mut pointer = std::ptr::null_mut::<VDS_PARTITION_PROP>();
        let mut count = 0_i32;
        let query = (Interface::vtable(&advanced).QueryPartitions)(
            Interface::as_raw(&advanced),
            &mut pointer,
            &mut count,
        );
        if let Err(error) = require_exact_success("query VDS partitions", query) {
            co_task_mem_free(pointer.cast::<c_void>());
            return Err(error);
        }
        if count < 0 || (count > 0 && pointer.is_null()) {
            co_task_mem_free(pointer.cast::<c_void>());
            return Err(StorageError::new(
                "query VDS partitions",
                "provider returned an invalid partition array",
            ));
        }
        let properties = if count == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(pointer, count as usize).to_vec()
        };
        co_task_mem_free(pointer.cast::<c_void>());
        properties
            .into_iter()
            .map(|property| {
                let (kind, active) = if property.PartitionStyle == VDS_PST_GPT {
                    let info = property.Anonymous.Gpt;
                    let kind = if info.partitionType == GPT_ESP {
                        PartitionKind::EfiSystem
                    } else if info.partitionType == GPT_MSR {
                        PartitionKind::MicrosoftReserved
                    } else if info.partitionType == GPT_RECOVERY {
                        PartitionKind::Recovery
                    } else {
                        PartitionKind::BasicData
                    };
                    (kind, false)
                } else if property.PartitionStyle == VDS_PST_MBR {
                    let info = property.Anonymous.Mbr;
                    let kind = match info.partitionType {
                        0xEF => PartitionKind::EfiSystem,
                        0x27 => PartitionKind::Recovery,
                        _ => PartitionKind::BasicData,
                    };
                    (kind, info.bootIndicator.0 != 0)
                } else {
                    return Err(StorageError::new(
                        "query VDS partitions",
                        "provider returned a partition with unsupported style",
                    ));
                };
                Ok(PartitionRecord {
                    partition_number: property.ulPartitionNumber,
                    offset_bytes: property.ullOffset,
                    size_bytes: property.ullSize,
                    kind,
                    active,
                })
            })
            .collect()
    }

    pub unsafe fn contiguous_free_bytes_after(
        disk_number: u32,
        end_offset_bytes: u64,
    ) -> Result<u64, StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let extents = free_extents(&disk.disk)?;
        extents
            .into_iter()
            .filter(|extent| extent.r#type == VDS_DET_FREE)
            .filter_map(|extent| (extent.ullOffset == end_offset_bytes).then_some(extent.ullSize))
            .max()
            .ok_or_else(|| {
                StorageError::new(
                    "query contiguous free space",
                    "no free extent immediately follows the partition",
                )
            })
    }

    pub unsafe fn current_free_extents(disk_number: u32) -> Result<Vec<FreeExtent>, StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let mut result = free_extents(&disk.disk)?
            .into_iter()
            .map(|extent| FreeExtent {
                offset_bytes: extent.ullOffset,
                length_bytes: extent.ullSize,
            })
            .collect::<Vec<_>>();
        result.sort_by_key(|extent| extent.offset_bytes);
        Ok(result)
    }

    /// Ask the disk driver to invalidate its cached partition table and re-enumerate the device.
    ///
    /// Microsoft documents `IOCTL_DISK_UPDATE_PROPERTIES` for synchronizing the system view after
    /// usable disk space changes, supports it since Windows XP, and permits it on a live volume.
    /// This is cache convergence only: callers must still trust the real VDS operation plus
    /// canonical IOCTL readback, and a failure here must not become a new preflight gate.
    unsafe fn update_disk_properties(disk_number: u32) -> Result<(), StorageError> {
        use windows::Win32::System::Ioctl::IOCTL_DISK_UPDATE_PROPERTIES;
        use windows::Win32::System::IO::DeviceIoControl;

        let (handle, _, _) = read_drive_layout(disk_number, false)?;
        let mut returned = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_UPDATE_PROPERTIES,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("update disk properties", error))?;
        Ok(())
    }

    pub unsafe fn set_mbr_signature(disk_number: u32, signature: u32) -> Result<(), StorageError> {
        use windows::Win32::System::Ioctl::{
            DRIVE_LAYOUT_INFORMATION_EX, IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
            IOCTL_DISK_UPDATE_PROPERTIES, PARTITION_STYLE_MBR,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        if signature == 0 {
            return Err(StorageError::new(
                "set MBR signature",
                "MBR signature must be non-zero",
            ));
        }
        let (handle, mut storage, returned) = read_drive_layout(disk_number, true)?;
        if returned < 48 {
            return Err(StorageError::new(
                "set MBR signature",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout = &mut *storage.as_mut_ptr().cast::<DRIVE_LAYOUT_INFORMATION_EX>();
        if layout.PartitionStyle != PARTITION_STYLE_MBR.0 as u32 {
            return Err(StorageError::new(
                "set MBR signature",
                "target disk is not MBR",
            ));
        }
        layout.Anonymous.Mbr.Signature = signature;
        let mut bytes = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
            Some(storage.as_ptr().cast()),
            returned,
            None,
            0,
            Some(&mut bytes),
            None,
        )
        .map_err(|error| api_error("set MBR drive layout", error))?;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_UPDATE_PROPERTIES,
            None,
            0,
            None,
            0,
            Some(&mut bytes),
            None,
        )
        .map_err(|error| api_error("refresh MBR drive layout", error))?;
        if mbr_signature(disk_number)? != Some(signature) {
            return Err(StorageError::new(
                "set MBR signature",
                "post-operation signature does not match the requested value",
            ));
        }
        Ok(())
    }

    unsafe fn read_drive_layout(
        disk_number: u32,
        writable: bool,
    ) -> Result<(OwnedHandle, Vec<u64>, u32), StorageError> {
        let access = if writable {
            0x8000_0000 | 0x4000_0000
        } else {
            GENERIC_READ.0
        };
        let (handle, _) =
            open_trusted_present_disk(disk_number, access, "open present physical disk layout")?;
        let (storage, returned) = read_drive_layout_from_handle(&handle)?;
        Ok((handle, storage, returned))
    }

    unsafe fn read_drive_layout_from_handle(
        handle: &OwnedHandle,
    ) -> Result<(Vec<u64>, u32), StorageError> {
        read_drive_layout_from_raw_handle(handle.0)
    }

    unsafe fn read_drive_layout_from_raw_handle(
        handle: HANDLE,
    ) -> Result<(Vec<u64>, u32), StorageError> {
        use windows::Win32::System::Ioctl::IOCTL_DISK_GET_DRIVE_LAYOUT_EX;
        use windows::Win32::System::IO::DeviceIoControl;

        let mut capacity = 4096usize;
        loop {
            if capacity > 4 * 1024 * 1024 {
                return Err(StorageError::new(
                    "read physical disk layout",
                    "drive layout exceeds the four-megabyte safety limit",
                ));
            }
            let mut storage = vec![0_u64; capacity.div_ceil(size_of::<u64>())];
            let mut returned = 0_u32;
            match DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                None,
                0,
                Some(storage.as_mut_ptr().cast()),
                (storage.len() * size_of::<u64>()) as u32,
                Some(&mut returned),
                None,
            ) {
                Ok(()) => return Ok((storage, returned)),
                Err(error) if is_variable_buffer_error(&error) => {
                    capacity = capacity.checked_mul(2).ok_or_else(|| {
                        StorageError::new("read physical disk layout", "buffer size overflow")
                    })?;
                }
                Err(error) => return Err(api_error("read physical disk layout", error)),
            }
        }
    }

    unsafe fn disk_length_from_handle(handle: &OwnedHandle) -> Result<u64, StorageError> {
        disk_length_from_raw_handle(handle.0)
    }

    unsafe fn disk_length_from_raw_handle(handle: HANDLE) -> Result<u64, StorageError> {
        use windows::Win32::System::Ioctl::{GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO};
        use windows::Win32::System::IO::DeviceIoControl;

        let mut length = GET_LENGTH_INFORMATION::default();
        let mut returned = 0u32;
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_LENGTH_INFO,
            None,
            0,
            Some((&mut length as *mut GET_LENGTH_INFORMATION).cast()),
            size_of::<GET_LENGTH_INFORMATION>() as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("read retained physical disk length", error))?;
        if returned < size_of::<GET_LENGTH_INFORMATION>() as u32 || length.Length <= 0 {
            return Err(StorageError::new(
                "read retained physical disk length",
                "disk length response is incomplete or invalid",
            ));
        }
        Ok(length.Length as u64)
    }

    /// Reads `STORAGE_DEVICE_DESCRIPTOR.BusType` from one already-open disk interface.
    ///
    /// Microsoft documents a header query followed by an allocation using the returned `Size`;
    /// using the two-call form avoids truncating descriptors on storage stacks that append
    /// bus-specific properties.
    unsafe fn disk_bus_type_from_handle(handle: HANDLE) -> Result<DiskBusType, StorageError> {
        use windows::Win32::Storage::FileSystem::BusTypeNvme;
        use windows::Win32::System::Ioctl::{
            PropertyStandardQuery, StorageDeviceProperty, IOCTL_STORAGE_QUERY_PROPERTY,
            STORAGE_DESCRIPTOR_HEADER, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut header = STORAGE_DESCRIPTOR_HEADER::default();
        let mut returned = 0_u32;
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut header as *mut _ as *mut c_void),
            size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query physical disk descriptor size", error))?;
        if returned < size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32 {
            return Err(StorageError::new(
                "query physical disk bus",
                "storage descriptor header was truncated",
            ));
        }
        let descriptor_size = usize::try_from(header.Size).map_err(|_| {
            StorageError::new(
                "query physical disk bus",
                "storage descriptor size does not fit in memory",
            )
        })?;
        if descriptor_size < size_of::<STORAGE_DEVICE_DESCRIPTOR>()
            || descriptor_size > MAX_DESCRIPTOR_BYTES
        {
            return Err(StorageError::new(
                "query physical disk bus",
                format!("invalid storage descriptor size: {descriptor_size}"),
            ));
        }

        let mut buffer = vec![0_u8; descriptor_size];
        returned = 0;
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buffer.as_mut_ptr() as *mut c_void),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query physical disk descriptor", error))?;
        if returned < size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32 {
            return Err(StorageError::new(
                "query physical disk bus",
                "storage device descriptor was truncated",
            ));
        }
        let descriptor =
            std::ptr::read_unaligned(buffer.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR);
        Ok(if descriptor.BusType == BusTypeNvme {
            DiskBusType::Nvme
        } else {
            DiskBusType::Other
        })
    }

    /// Reads and reconciles `STORAGE_DEVICE_DESCRIPTOR.BusType` through every present opaque
    /// SetupAPI path that maps to the current disk number.
    pub unsafe fn disk_bus_type(disk_number: u32) -> Result<DiskBusType, StorageError> {
        let interfaces = present_physical_disk_interfaces()?
            .into_iter()
            .filter(|interface| interface.disk_number == disk_number)
            .collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(interfaces.len());
        let mut bus_types = Vec::with_capacity(interfaces.len());
        for interface in interfaces {
            let handle = open_present_disk_interface_path(
                &interface.device_path,
                GENERIC_READ.0,
                "open present physical disk for bus query",
            )?;
            verify_opened_present_disk_number(
                handle.0,
                disk_number,
                "verify present physical disk for bus query",
            )?;
            snapshots.push((
                interface.device_path.clone(),
                disk_layout_snapshot_from_handle(handle.0)?,
            ));
            bus_types.push((interface.device_path, disk_bus_type_from_handle(handle.0)?));
        }
        // Capacity, disk identity and canonical partition layout must agree before the auxiliary
        // bus property is allowed to influence install defaults.
        let _ = reconcile_present_disk_snapshots(disk_number, snapshots)?;
        reconcile_present_disk_bus_types(disk_number, bus_types)
    }

    pub unsafe fn physical_disk_sector_geometry(
        disk_number: u32,
    ) -> Result<DiskSectorGeometry, StorageError> {
        use windows::Win32::System::Ioctl::{
            PropertyStandardQuery, StorageAccessAlignmentProperty, IOCTL_STORAGE_QUERY_PROPERTY,
            STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        let (handle, _) = open_trusted_present_disk(
            disk_number,
            GENERIC_READ.0,
            "open present physical disk for sector geometry query",
        )?;
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageAccessAlignmentProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut descriptor = STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR::default();
        let mut returned = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some((&mut descriptor as *mut STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR).cast()),
            size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>() as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query physical disk sector geometry", error))?;
        validated_disk_sector_geometry(
            descriptor.Version,
            descriptor.Size,
            returned,
            size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>() as u32,
            descriptor.BytesPerLogicalSector,
            descriptor.BytesPerPhysicalSector,
            descriptor.BytesOffsetForSectorAlignment,
        )
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn api_error(operation: &'static str, error: windows::core::Error) -> StorageError {
        StorageError::new(
            operation,
            format!("{} (HRESULT 0x{:08X})", error, error.code().0 as u32),
        )
    }

    fn hresult_error(operation: &'static str, result: HRESULT) -> StorageError {
        StorageError::new(
            operation,
            format!(
                "{} (HRESULT 0x{:08X})",
                windows::core::Error::from(result),
                result.0 as u32
            ),
        )
    }

    fn require_exact_success(operation: &'static str, result: HRESULT) -> Result<(), StorageError> {
        if result == HRESULT(0) {
            Ok(())
        } else {
            Err(hresult_error(operation, result))
        }
    }

    unsafe fn free_extents(disk: &IVdsDisk) -> Result<Vec<VDS_DISK_EXTENT>, StorageError> {
        let mut pointer = std::ptr::null_mut();
        let mut count = 0;
        let query = (Interface::vtable(disk).QueryExtents)(
            Interface::as_raw(disk),
            &mut pointer,
            &mut count,
        );
        if let Err(error) = require_exact_success("query VDS disk extents", query) {
            co_task_mem_free(pointer.cast::<c_void>());
            return Err(error);
        }
        if count < 0 || (count > 0 && pointer.is_null()) {
            co_task_mem_free(pointer.cast::<c_void>());
            return Err(StorageError::new(
                "query VDS disk extents",
                "provider returned an invalid extent array",
            ));
        }
        let values = if count == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(pointer, count as usize).to_vec()
        };
        co_task_mem_free(pointer.cast::<c_void>());
        let mut values: Vec<_> = values
            .into_iter()
            .filter(|extent| extent.r#type == VDS_DET_FREE)
            .collect();
        values.sort_by_key(|extent| (extent.ullOffset, extent.ullSize));
        Ok(values)
    }

    unsafe fn provider_default_free_extents(
        disk: &IVdsDisk,
    ) -> Result<Vec<VDS_DISK_FREE_EXTENT>, StorageError> {
        let disk3 = disk
            .cast::<IVdsDisk3>()
            .map_err(|error| api_error("open VDS provider-default free-extent interface", error))?;
        let mut pointer = std::ptr::null_mut();
        let mut count = 0_i32;
        let result = (Interface::vtable(&disk3).QueryFreeExtents)(
            Interface::as_raw(&disk3),
            VDS_PROVIDER_DEFAULT_ALIGNMENT,
            &mut pointer,
            &mut count,
        );
        // Microsoft documents S_FALSE as the successful empty-set result.
        if result != HRESULT(0) && result != HRESULT(1) {
            co_task_mem_free(pointer.cast::<c_void>());
            return Err(hresult_error(
                "query VDS provider-default free extents",
                result,
            ));
        }
        if count < 0 || (count > 0 && pointer.is_null()) {
            co_task_mem_free(pointer.cast::<c_void>());
            return Err(StorageError::new(
                "query VDS provider-default free extents",
                "provider returned an invalid extent array",
            ));
        }
        let mut values = if count == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(pointer, count as usize).to_vec()
        };
        co_task_mem_free(pointer.cast::<c_void>());
        values.sort_by_key(|extent| (extent.ullOffset, extent.ullSize));
        Ok(values)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SelectedFreeExtent {
        offset_bytes: u64,
        requested_size: u64,
        minimum_size: u64,
        /// Hard lower bound authorized by the caller and the raw current free extent.
        ///
        /// This is deliberately distinct from `offset_bytes`, which is only the offset passed to
        /// `CreatePartitionEx`. With `ulAlign == 0`, VDS may round that requested offset either up
        /// or down. An automatic-layout request may therefore legitimately return an offset below
        /// the provider-default suggestion, while an explicit caller offset/envelope remains a
        /// hard authorization boundary.
        authorized_start_bytes: u64,
        /// Selection evidence used only in diagnostics. For inventory-selected requests this is
        /// the VDS raw extent; for an explicit caller envelope it is that canonical IOCTL-proven
        /// free range because stale VDS inventory is deliberately not a gate.
        raw_offset_bytes: u64,
        raw_size_bytes: u64,
        /// Provider-aligned selection evidence used only in diagnostics. Explicit-envelope
        /// requests repeat the caller range here and let the real provider operation decide its
        /// legal alignment, with canonical readback enforcing the hard bounds.
        provider_offset_bytes: u64,
        provider_size_bytes: u64,
        authorized_end_bytes: u64,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ExpectedPartitionRole {
        Gpt {
            partition_type: [u8; 16],
            /// Offline block-move reconstruction explicitly asks to preserve these values.
            /// Ordinary GPT creation leaves this `None`; a provider-selected partition ID and
            /// non-role attributes must not become a generic creation failure.
            preserved_identity: Option<([u8; 16], u64)>,
        },
        Mbr {
            partition_type: u8,
            boot_indicator: bool,
        },
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ObservedCreatedPartition {
        created: CreatedPartition,
        token: DiskLayoutPartitionToken,
    }

    /// Compare the same newly-created partition across formatting and access-path assignment.
    ///
    /// Microsoft documents that `IVdsVolumeMF::AddAccessPath` may update the basic-data
    /// `GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER` bit, including on its successful `S_FALSE`
    /// result. The exact extent, partition type and GPT partition identifier remain the identity;
    /// every other GPT attribute transition and every MBR metadata change remains a mismatch.
    fn same_created_partition_after_access_path(
        before: ObservedCreatedPartition,
        after: ObservedCreatedPartition,
    ) -> bool {
        if before.created != after.created {
            return false;
        }
        match (before.token, after.token) {
            (
                DiskLayoutPartitionToken::Gpt {
                    partition_type: before_type,
                    partition_id: before_id,
                    attributes: before_attributes,
                },
                DiskLayoutPartitionToken::Gpt {
                    partition_type: after_type,
                    partition_id: after_id,
                    attributes: after_attributes,
                },
            ) => {
                before_type == after_type
                    && before_id == after_id
                    && (before_attributes == after_attributes
                        || (before_type == guid_identity(GPT_BASIC_DATA)
                            && ((before_attributes ^ after_attributes)
                                & !GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER.0)
                                == 0))
            }
            (before_token, after_token) => before_token == after_token,
        }
    }

    fn expected_partition_role(
        token: DiskLayoutPartitionToken,
        preserve_gpt_metadata: bool,
    ) -> ExpectedPartitionRole {
        match token {
            DiskLayoutPartitionToken::Gpt {
                partition_type,
                partition_id,
                attributes,
            } => ExpectedPartitionRole::Gpt {
                partition_type,
                preserved_identity: preserve_gpt_metadata.then_some((partition_id, attributes)),
            },
            DiskLayoutPartitionToken::Mbr {
                partition_type,
                boot_indicator,
            } => ExpectedPartitionRole::Mbr {
                partition_type,
                boot_indicator,
            },
        }
    }

    fn partition_role_violation(
        actual: DiskLayoutPartitionToken,
        expected: ExpectedPartitionRole,
    ) -> Option<String> {
        match (actual, expected) {
            (
                DiskLayoutPartitionToken::Gpt {
                    partition_type: actual_type,
                    partition_id,
                    attributes,
                },
                ExpectedPartitionRole::Gpt {
                    partition_type,
                    preserved_identity,
                },
            ) if actual_type == partition_type => {
                if let Some((expected_id, expected_attributes)) = preserved_identity {
                    if partition_id != expected_id || attributes != expected_attributes {
                        return Some(
                            "recreated GPT partition did not preserve its requested identifier/attributes"
                                .to_owned(),
                        );
                    }
                }
                None
            }
            (
                DiskLayoutPartitionToken::Mbr {
                    partition_type: actual_type,
                    boot_indicator: actual_boot,
                },
                ExpectedPartitionRole::Mbr {
                    partition_type,
                    boot_indicator,
                },
            ) if actual_type == partition_type && actual_boot == boot_indicator => None,
            (actual, expected) => Some(format!(
                "created partition role differs from the request: actual={actual:?} expected={expected:?}"
            )),
        }
    }

    fn preserved_gpt_metadata_violation(
        actual: &GptPartitionMetadata,
        expected: &GptPartitionMetadata,
    ) -> Option<String> {
        if actual.partition_id != expected.partition_id {
            return Some(
                "recreated GPT partition identifier differs from the requested value".into(),
            );
        }
        if actual.attributes != expected.attributes {
            return Some(
                "recreated GPT partition attributes differ from the requested value".into(),
            );
        }
        if actual.name != expected.name {
            return Some("recreated GPT partition name differs from the requested value".into());
        }
        None
    }

    unsafe fn verify_preserved_gpt_metadata(
        disk_number: u32,
        created: CreatedPartition,
        expected: Option<&GptPartitionMetadata>,
    ) -> Result<(), StorageError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual = gpt_partition_metadata_at(disk_number, created)?;
        if let Some(detail) = preserved_gpt_metadata_violation(&actual, expected) {
            return Err(StorageError::new("verify preserved GPT metadata", detail));
        }
        Ok(())
    }

    fn select_free_extent(
        raw_extents: &[VDS_DISK_EXTENT],
        provider_extents: &[VDS_DISK_FREE_EXTENT],
        requested_offset: u64,
        requested_size: u64,
        authorization: Option<FreeExtent>,
        minimum_size: u64,
    ) -> Result<SelectedFreeExtent, StorageError> {
        if requested_size == 0 || minimum_size == 0 || minimum_size > requested_size {
            return Err(StorageError::new(
                "select free extent",
                "partition request and minimum capacity are inconsistent",
            ));
        }
        // `ullOffset` and `ullSize` are desired geometry, not an authorization envelope. Microsoft
        // explicitly documents that `CreatePartitionEx(..., ulAlign = 0, ...)` may round the
        // offset up or down and that `VDS_ASYNC_OUTPUT.cp.ullOffset` is the actual offset, which may
        // differ from `ullOffset`. Still reject an arithmetically impossible desired range before
        // entering COM; the computed end must never be reused as a post-create hard boundary.
        if requested_offset != 0 {
            requested_offset
                .checked_add(requested_size)
                .ok_or_else(|| {
                    StorageError::new("select free extent", "requested partition end overflow")
                })?;
        }
        let (authorized_start, authorization_end) = if let Some(authorization) = authorization {
            if authorization.length_bytes == 0 {
                return Err(StorageError::new(
                    "select free extent",
                    "partition authorization envelope is empty",
                ));
            }
            let end = authorization
                .offset_bytes
                .checked_add(authorization.length_bytes)
                .ok_or_else(|| {
                    StorageError::new(
                        "select free extent",
                        "partition authorization envelope end overflow",
                    )
                })?;
            if requested_offset != 0
                && (requested_offset < authorization.offset_bytes || requested_offset >= end)
            {
                return Err(StorageError::new(
                    "select free extent",
                    "explicit partition lower bound is outside the authorization envelope",
                ));
            }
            (Some(authorization.offset_bytes), Some(end))
        } else {
            (None, None)
        };
        for raw in raw_extents {
            let raw_end = raw
                .ullOffset
                .checked_add(raw.ullSize)
                .ok_or_else(|| StorageError::new("select free extent", "extent end overflow"))?;
            let requested_start = if requested_offset != 0 {
                requested_offset
            } else {
                authorized_start.unwrap_or(raw.ullOffset)
            };
            if requested_start < raw.ullOffset || requested_start >= raw_end {
                continue;
            }
            for provider in provider_extents {
                let provider_end = provider
                    .ullOffset
                    .checked_add(provider.ullSize)
                    .ok_or_else(|| {
                        StorageError::new("select free extent", "provider extent end overflow")
                    })?;
                if provider_end <= raw.ullOffset
                    || provider.ullOffset >= raw_end
                    || requested_start >= provider_end
                {
                    continue;
                }
                let start = requested_start.max(provider.ullOffset);
                let hard_requested_start = authorized_start.unwrap_or(raw.ullOffset);
                let authorized_start = raw.ullOffset.max(hard_requested_start);
                let mut authorized_end = raw_end.min(provider_end);
                if let Some(envelope_end) = authorization_end {
                    authorized_end = authorized_end.min(envelope_end);
                }
                if start >= authorized_end {
                    continue;
                }
                let available = authorized_end - start;
                // The requested capacity, safety minimum and authorization envelope are distinct.
                // QueryFreeExtents(0) may move the legal start forward. Use the requested capacity
                // when it still fits, otherwise only the real remainder, and never write unless
                // that remainder still satisfies the caller's separately supplied minimum.
                let size = requested_size.min(available);
                if size < minimum_size {
                    continue;
                }
                return Ok(SelectedFreeExtent {
                    offset_bytes: start,
                    requested_size: size,
                    minimum_size,
                    authorized_start_bytes: authorized_start,
                    raw_offset_bytes: raw.ullOffset,
                    raw_size_bytes: raw.ullSize,
                    provider_offset_bytes: provider.ullOffset,
                    provider_size_bytes: provider.ullSize,
                    authorized_end_bytes: authorized_end,
                });
            }
        }
        Err(StorageError::new(
            "select free extent",
            "no raw/provider-default free-extent intersection can satisfy the authorized range",
        ))
    }

    /// Select desired geometry directly from a canonical caller authorization envelope.
    ///
    /// `IVdsDisk::QueryExtents` and `IVdsDisk3::QueryFreeExtents` are useful inventory APIs, but
    /// neither is a prerequisite in the `CreatePartitionEx` contract. After a committed Shrink,
    /// both can temporarily expose a stale provider view even though current IOCTL layout and the
    /// source-volume extent already prove the exact reclaimed tail. In that case querying them as
    /// a second hard gate only strands a still-reversible operation. The real create call remains
    /// authoritative and its one-delta canonical readback must stay inside this envelope.
    fn select_caller_authorized_extent(
        snapshot: &DiskLayoutSnapshot,
        requested_offset: u64,
        requested_size: u64,
        authorization: FreeExtent,
        minimum_size: u64,
    ) -> Result<SelectedFreeExtent, StorageError> {
        if requested_size == 0 || minimum_size == 0 || minimum_size > requested_size {
            return Err(StorageError::new(
                "select authorized extent",
                "partition request and minimum capacity are inconsistent",
            ));
        }
        let authorized_end = authorization
            .offset_bytes
            .checked_add(authorization.length_bytes)
            .ok_or_else(|| {
                StorageError::new(
                    "select authorized extent",
                    "partition authorization envelope end overflow",
                )
            })?;
        if authorization.length_bytes == 0 || authorized_end > snapshot.disk_size_bytes {
            return Err(StorageError::new(
                "select authorized extent",
                "partition authorization envelope is empty or outside the current disk",
            ));
        }
        for partition in &snapshot.partitions {
            let partition_end = partition
                .offset_bytes
                .checked_add(partition.size_bytes)
                .ok_or_else(|| {
                    StorageError::new("select authorized extent", "current partition end overflow")
                })?;
            if authorization.offset_bytes < partition_end && partition.offset_bytes < authorized_end
            {
                return Err(StorageError::new(
                    "select authorized extent",
                    "partition authorization envelope overlaps the current canonical layout",
                ));
            }
        }
        let start = if requested_offset == 0 {
            authorization.offset_bytes
        } else {
            requested_offset
        };
        if start < authorization.offset_bytes || start >= authorized_end {
            return Err(StorageError::new(
                "select authorized extent",
                "explicit partition start is outside the authorization envelope",
            ));
        }
        if requested_offset != 0 {
            requested_offset
                .checked_add(requested_size)
                .ok_or_else(|| {
                    StorageError::new(
                        "select authorized extent",
                        "requested partition end overflow",
                    )
                })?;
        }
        let available = authorized_end - start;
        let size = requested_size.min(available);
        if size < minimum_size {
            return Err(StorageError::new(
                "select authorized extent",
                "authorization envelope cannot satisfy the partition minimum",
            ));
        }
        Ok(SelectedFreeExtent {
            offset_bytes: start,
            requested_size: size,
            minimum_size,
            authorized_start_bytes: authorization.offset_bytes,
            raw_offset_bytes: authorization.offset_bytes,
            raw_size_bytes: authorization.length_bytes,
            provider_offset_bytes: authorization.offset_bytes,
            provider_size_bytes: authorization.length_bytes,
            authorized_end_bytes: authorized_end,
        })
    }

    fn logical_sector_create_attempt_sizes(
        selected: SelectedFreeExtent,
        logical_sector_bytes: u32,
    ) -> Result<Vec<u64>, StorageError> {
        let sector = u64::from(logical_sector_bytes);
        if sector == 0 || !logical_sector_bytes.is_power_of_two() {
            return Err(StorageError::new(
                "prepare VDS partition geometry",
                "the current disk logical sector size is zero or not a power of two",
            ));
        }

        // Microsoft specifies `CreatePartitionEx.ullSize` in bytes and permits the provider to
        // adjust an offset when `ulAlign` is zero. Therefore the caller's exact byte request is the
        // first attempt and neither its offset nor size is an independent preflight gate. Some
        // real VDS providers nevertheless return E_INVALIDARG before creating an async object for
        // a non-sector-sized capacity. Only after canonical layout readback proves that rejection
        // changed nothing may the caller try the current device's whole-sector representation.
        let available = selected
            .authorized_end_bytes
            .checked_sub(selected.offset_bytes)
            .ok_or_else(|| {
                StorageError::new(
                    "prepare VDS partition geometry",
                    "the authorized range ends before the requested partition start",
                )
            })?;
        let minimum_remainder = selected.minimum_size % sector;
        let minimum = if minimum_remainder == 0 {
            selected.minimum_size
        } else {
            selected
                .minimum_size
                .checked_add(sector - minimum_remainder)
                .ok_or_else(|| {
                    StorageError::new(
                        "prepare VDS partition geometry",
                        "sector-rounded minimum partition capacity overflowed",
                    )
                })?
        };
        let maximum = available - (available % sector);
        if maximum < minimum {
            return Err(StorageError::new(
                "prepare VDS partition geometry",
                "the authorized range cannot contain the caller minimum in whole logical sectors",
            ));
        }

        let desired_remainder = selected.requested_size % sector;
        let desired_rounded_up = if desired_remainder == 0 {
            selected.requested_size
        } else {
            selected
                .requested_size
                .checked_add(sector - desired_remainder)
                .ok_or_else(|| {
                    StorageError::new(
                        "prepare VDS partition geometry",
                        "sector-rounded desired partition capacity overflowed",
                    )
                })?
        };
        let sector_compatible_desired = desired_rounded_up.min(maximum);
        let mut sizes = vec![selected.requested_size];
        if sector_compatible_desired != selected.requested_size {
            sizes.push(sector_compatible_desired);
        }
        if !sizes.contains(&minimum) {
            sizes.push(minimum);
        }
        Ok(sizes)
    }

    fn canonical_vds_partition_style(
        snapshot: &DiskLayoutSnapshot,
    ) -> Result<VDS_PARTITION_STYLE, StorageError> {
        // DRIVE_LAYOUT_INFORMATION_EX::PartitionStyle is the documented current on-disk fact.
        // VDS_DISK_PROP is provider inventory and can remain stale even after Refresh: a real
        // Hyper-V GPT disk was returned as another style here, which selected the wrong union arm.
        // Use VDS only to execute the operation; derive every style-dependent input from the
        // canonical IOCTL snapshot. ulAlign remains the independent provider-default value zero.
        match snapshot.disk {
            StableDiskIdentity::Gpt { .. } => Ok(VDS_PST_GPT),
            StableDiskIdentity::Mbr { .. } => Ok(VDS_PST_MBR),
            StableDiskIdentity::Raw => Err(StorageError::new(
                "create partition",
                "the canonical drive layout reports an uninitialized disk",
            )),
        }
    }

    fn may_retry_create_after_invalid_argument(
        result: HRESULT,
        async_pointer_is_null: bool,
        current_attempt: usize,
        attempt_count: usize,
    ) -> bool {
        result == E_INVALIDARG
            && async_pointer_is_null
            && current_attempt
                .checked_add(1)
                .is_some_and(|next| next < attempt_count)
    }

    fn created_extent_selection_violation(
        created: DiskLayoutPartitionSnapshot,
        selected: SelectedFreeExtent,
        expected_role: ExpectedPartitionRole,
    ) -> Option<String> {
        let Some(created_end) = created.offset_bytes.checked_add(created.size_bytes) else {
            return Some(format!(
                "created extent end overflow: actual_offset={} actual_size={}",
                created.offset_bytes, created.size_bytes
            ));
        };
        if let Some(violation) = partition_role_violation(created.token, expected_role) {
            return Some(violation);
        }
        if created.size_bytes < selected.minimum_size {
            return Some(format!(
                "created partition is below the caller minimum: actual_size={} minimum={} requested_size={}",
                created.size_bytes, selected.minimum_size, selected.requested_size
            ));
        }
        if created.offset_bytes < selected.authorized_start_bytes {
            return Some(format!(
                "created partition starts before the authorized range: actual_offset={} authorized_start={} provider_request_offset={} selection_evidence_offset={}",
                created.offset_bytes,
                selected.authorized_start_bytes,
                selected.offset_bytes,
                selected.raw_offset_bytes
            ));
        }
        if created_end > selected.authorized_end_bytes {
            return Some(format!(
                "created partition ends after the authorized range: actual_end={} authorized_end={} actual_offset={} actual_size={} alignment_evidence_offset={} alignment_evidence_size={}",
                created_end,
                selected.authorized_end_bytes,
                created.offset_bytes,
                created.size_bytes,
                selected.provider_offset_bytes,
                selected.provider_size_bytes
            ));
        }
        None
    }

    #[cfg(test)]
    fn created_extent_satisfies_selection(
        created: DiskLayoutPartitionSnapshot,
        selected: SelectedFreeExtent,
        expected_role: ExpectedPartitionRole,
    ) -> bool {
        created_extent_selection_violation(created, selected, expected_role).is_none()
    }

    /// Reconcile an error reported after `CreatePartitionEx` has been started.
    ///
    /// VDS asynchronous failure does not prove that no topology change occurred. The current
    /// canonical layout decides whether there is nothing to undo, one exact provider-created
    /// contained extent that can be deleted, or an ambiguous partial state that must be preserved
    /// for diagnosis. The request offset is never used as a guessed deletion target.
    fn reconcile_started_partition_creation<R, D>(
        primary: StorageError,
        baseline: &DiskLayoutSnapshot,
        selected: SelectedFreeExtent,
        expected_role: ExpectedPartitionRole,
        read_current: R,
        delete_exact: D,
    ) -> Result<CreatedPartition, StorageError>
    where
        R: FnOnce() -> Result<DiskLayoutSnapshot, StorageError>,
        D: FnOnce(ObservedCreatedPartition) -> Result<(), StorageError>,
    {
        let current = match read_current() {
            Ok(current) => current,
            Err(readback) => {
                return Err(StorageError::new(
                    primary.operation,
                    format!(
                        "{}; partition creation may have committed, but current layout readback failed (partial state): {}",
                        primary.detail, readback
                    ),
                ));
            }
        };
        if same_partition_layout(baseline, &current) {
            return Err(primary);
        }
        let Some(created) = created_partition_delta(baseline, &current) else {
            return Err(StorageError::new(
                primary.operation,
                format!(
                    "{}; partition creation may have committed and the current layout contains an additional or ambiguous change (partial state)",
                    primary.detail
                ),
            ));
        };
        if let Some(violation) =
            created_extent_selection_violation(created, selected, expected_role)
        {
            return Err(StorageError::new(
                primary.operation,
                format!(
                    "{}; partition creation produced an unauthorized or uncontained extent (partial state): {}",
                    primary.detail, violation
                ),
            ));
        }
        let created = ObservedCreatedPartition {
            created: CreatedPartition {
                offset_bytes: created.offset_bytes,
                size_bytes: created.size_bytes,
            },
            token: created.token,
        };
        match delete_exact(created) {
            Ok(()) => Err(StorageError::new(
                primary.operation,
                format!(
                    "{}; exact provider-created partition was rolled back",
                    primary.detail
                ),
            )),
            Err(cleanup) => Err(StorageError::new(
                primary.operation,
                format!(
                    "{}; exact provider-created partition rollback failed (partial state): {}",
                    primary.detail, cleanup
                ),
            )),
        }
    }

    unsafe fn create_parameters(
        style: VDS_PARTITION_STYLE,
        kind: PartitionKind,
        active: bool,
        label: &str,
        preserved_gpt: Option<&GptPartitionMetadata>,
    ) -> Result<CREATE_PARTITION_PARAMETERS, StorageError> {
        if preserved_gpt.is_some() && (style != VDS_PST_GPT || kind != PartitionKind::BasicData) {
            return Err(StorageError::new(
                "create partition",
                "preserved GPT metadata is valid only for a GPT basic-data partition",
            ));
        }
        if style == VDS_PST_GPT {
            let partition_type = match kind {
                PartitionKind::BasicData => GPT_BASIC_DATA,
                PartitionKind::EfiSystem => GPT_ESP,
                PartitionKind::MicrosoftReserved => GPT_MSR,
                PartitionKind::Recovery => GPT_RECOVERY,
            };
            let (partition_id, attributes, name) = if let Some(metadata) = preserved_gpt {
                let bytes = metadata.partition_id;
                (
                    GUID::from_values(
                        u32::from_le_bytes(bytes[0..4].try_into().expect("GUID data1")),
                        u16::from_le_bytes(bytes[4..6].try_into().expect("GUID data2")),
                        u16::from_le_bytes(bytes[6..8].try_into().expect("GUID data3")),
                        bytes[8..16].try_into().expect("GUID data4"),
                    ),
                    metadata.attributes,
                    metadata.name,
                )
            } else {
                let mut name = [0_u16; 36];
                for (target, value) in name.iter_mut().zip(label.encode_utf16()) {
                    *target = value;
                }
                (
                    CoCreateGuid()
                        .map_err(|error| api_error("create GPT partition identifier", error))?,
                    0,
                    name,
                )
            };
            return Ok(CREATE_PARTITION_PARAMETERS {
                style,
                Anonymous: CREATE_PARTITION_PARAMETERS_0 {
                    GptPartInfo: CREATE_PARTITION_PARAMETERS_0_0 {
                        partitionType: partition_type,
                        partitionId: partition_id,
                        attributes,
                        name,
                    },
                },
            });
        }
        if style == VDS_PST_MBR {
            let partition_type = match kind {
                PartitionKind::BasicData | PartitionKind::Recovery => 0x07,
                PartitionKind::EfiSystem => 0xEF,
                PartitionKind::MicrosoftReserved => {
                    return Err(StorageError::new(
                        "create MBR partition",
                        "Microsoft reserved partitions require GPT",
                    ))
                }
            };
            return Ok(CREATE_PARTITION_PARAMETERS {
                style,
                Anonymous: CREATE_PARTITION_PARAMETERS_0 {
                    MbrPartInfo: CREATE_PARTITION_PARAMETERS_0_1 {
                        partitionType: partition_type,
                        bootIndicator: BOOLEAN(u8::from(active)),
                    },
                },
            });
        }
        Err(StorageError::new(
            "create partition",
            "disk is not initialized as MBR or GPT",
        ))
    }

    unsafe fn format_volume(
        volume: &IVdsVolume,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let label = wide(&options.label);
        let filesystem = wide(options.file_system.name());
        if let Ok(formatter) = volume.cast::<IVdsVolumeMF2>() {
            let mut raw = std::ptr::null_mut();
            let start = (Interface::vtable(&formatter).FormatEx)(
                Interface::as_raw(&formatter),
                PCWSTR(filesystem.as_ptr()),
                0,
                options.allocation_unit_size,
                PCWSTR(label.as_ptr()),
                BOOL::from(options.force_dismount),
                BOOL::from(options.quick),
                BOOL::from(false),
                &mut raw,
            );
            let asynchronous = exact_async_interface("start VDS volume format", start, raw)?;
            wait_async("format volume", &asynchronous, Some(VDS_ASYNCOUT_FORMAT))?;
            return Ok(());
        }
        if options.file_system == FileSystem::ExFat {
            return Err(StorageError::new(
                "format volume",
                "the installed VDS provider does not expose IVdsVolumeMF2 required for exFAT",
            ));
        }
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume formatter", error))?;
        let fs = match options.file_system {
            FileSystem::Ntfs => VDS_FST_NTFS,
            FileSystem::Fat => VDS_FST_FAT,
            FileSystem::Fat32 => VDS_FST_FAT32,
            FileSystem::ExFat => VDS_FST_EXFAT,
        };
        let mut raw = std::ptr::null_mut();
        let start = (Interface::vtable(&formatter).Format)(
            Interface::as_raw(&formatter),
            fs,
            PCWSTR(label.as_ptr()),
            options.allocation_unit_size,
            BOOL::from(options.force_dismount),
            BOOL::from(options.quick),
            BOOL::from(false),
            &mut raw,
        );
        let asynchronous = exact_async_interface("start VDS volume format", start, raw)?;
        wait_async("format volume", &asynchronous, Some(VDS_ASYNCOUT_FORMAT))?;
        Ok(())
    }

    unsafe fn format_partition(
        disk: &IVdsDisk,
        offset_bytes: u64,
        file_system: FileSystem,
        label: &str,
    ) -> Result<(), StorageError> {
        let formatter = disk
            .cast::<IVdsDiskPartitionMF>()
            .map_err(|error| api_error("open VDS partition formatter", error))?;
        let filesystem = wide(file_system.name());
        let label = wide(label);
        let mut raw = std::ptr::null_mut();
        let start = (Interface::vtable(&formatter).FormatPartitionEx)(
            Interface::as_raw(&formatter),
            offset_bytes,
            PCWSTR(filesystem.as_ptr()),
            0,
            0,
            PCWSTR(label.as_ptr()),
            BOOL::from(false),
            BOOL::from(true),
            BOOL::from(false),
            &mut raw,
        );
        let asynchronous = exact_async_interface("start VDS partition format", start, raw)?;
        wait_async("format partition", &asynchronous, Some(VDS_ASYNCOUT_FORMAT))?;
        Ok(())
    }

    pub(super) fn classify_add_access_path_result(result: HRESULT) -> Result<bool, StorageError> {
        if result == HRESULT(0) {
            Ok(false)
        } else if result == S_FALSE_HRESULT {
            // Microsoft documents S_FALSE specifically for AddAccessPath as a successful mount
            // whose secondary GPT NO_DRIVE_LETTER/default-share update was incomplete.  It is not
            // accepted as proof by itself: the caller must still open the assigned letter and
            // read back the exact current physical extent before continuing.
            Ok(true)
        } else {
            Err(hresult_error("assign drive letter", result))
        }
    }

    pub(super) fn verify_added_access_path_with<R, S>(
        expected: VolumeIdentity,
        mut readback: R,
        mut wait: S,
    ) -> Result<(), StorageError>
    where
        R: FnMut() -> Result<VolumeIdentity, StorageError>,
        S: FnMut(),
    {
        const ATTEMPTS: usize = 50;
        let mut last_error = None;
        for attempt in 0..ATTEMPTS {
            match readback() {
                Ok(actual) if same_volume_identity(actual, expected) => return Ok(()),
                Ok(actual) => {
                    return Err(StorageError::new(
                        "verify assigned drive letter",
                        format!(
                            "assigned drive letter resolves to disk {} offset {} length {}, expected disk {} offset {} length {}",
                            actual.disk_number,
                            actual.offset_bytes,
                            actual.extent_length_bytes,
                            expected.disk_number,
                            expected.offset_bytes,
                            expected.extent_length_bytes
                        ),
                    ));
                }
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < ATTEMPTS {
                wait();
            }
        }
        Err(StorageError::new(
            "verify assigned drive letter",
            format!(
                "the assigned drive letter did not become readable within 5 seconds; last error: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "no readback result".to_owned())
            ),
        ))
    }

    unsafe fn add_access_path(
        volume: &IVdsVolume,
        drive_letter: char,
        expected: VolumeIdentity,
    ) -> Result<(), StorageError> {
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume access-path interface", error))?;
        let drive_letter = normalize_letter(drive_letter)?;
        let path = wide(&format!("{drive_letter}:\\"));
        let result = (Interface::vtable(&formatter).AddAccessPath)(
            Interface::as_raw(&formatter),
            PCWSTR(path.as_ptr()),
        );
        let incomplete_secondary_update = classify_add_access_path_result(result)?;
        verify_added_access_path_with(
            expected,
            || volume_identity(drive_letter),
            || std::thread::sleep(std::time::Duration::from_millis(100)),
        )?;
        if incomplete_secondary_update {
            log::warn!(
                "VDS assigned drive letter {drive_letter}: with S_FALSE; exact extent readback succeeded, but the GPT no-drive-letter attribute or default share may not have been updated"
            );
        }
        Ok(())
    }

    unsafe fn delete_access_path(
        volume: &IVdsVolume,
        drive_letter: char,
        force: bool,
    ) -> Result<(), StorageError> {
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume access-path interface", error))?;
        let path = wide(&format!("{}:\\", normalize_letter(drive_letter)?));
        let result = (Interface::vtable(&formatter).DeleteAccessPath)(
            Interface::as_raw(&formatter),
            PCWSTR(path.as_ptr()),
            BOOL::from(force),
        );
        require_exact_success("remove drive letter access path", result)
    }

    unsafe fn open_disk_for_initialization(disk_number: u32) -> Result<OwnedHandle, StorageError> {
        open_trusted_present_disk(
            disk_number,
            0x8000_0000 | 0x4000_0000,
            "open present physical disk for initialization",
        )
        .map(|(handle, _)| handle)
    }

    unsafe fn initialize_disk_ioctl(
        handle: &OwnedHandle,
        style: DiskStyle,
    ) -> Result<(), StorageError> {
        use windows::Win32::System::Ioctl::{
            CREATE_DISK, CREATE_DISK_0, CREATE_DISK_GPT, CREATE_DISK_MBR, IOCTL_DISK_CREATE_DISK,
            IOCTL_DISK_UPDATE_PROPERTIES, PARTITION_STYLE_GPT, PARTITION_STYLE_MBR,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        let guid = CoCreateGuid().map_err(|error| api_error("create disk identifier", error))?;
        let signature =
            u32::from_le_bytes(guid.to_u128().to_le_bytes()[..4].try_into().unwrap()).max(1);
        let create = match style {
            DiskStyle::Mbr => CREATE_DISK {
                PartitionStyle: PARTITION_STYLE_MBR,
                Anonymous: CREATE_DISK_0 {
                    Mbr: CREATE_DISK_MBR {
                        Signature: signature,
                    },
                },
            },
            DiskStyle::Gpt => CREATE_DISK {
                PartitionStyle: PARTITION_STYLE_GPT,
                Anonymous: CREATE_DISK_0 {
                    Gpt: CREATE_DISK_GPT {
                        DiskId: guid,
                        MaxPartitionCount: 128,
                    },
                },
            },
        };
        let mut returned = 0;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_CREATE_DISK,
            Some((&create as *const CREATE_DISK).cast::<c_void>()),
            size_of::<CREATE_DISK>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("initialize disk partition table", error))?;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_UPDATE_PROPERTIES,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("update disk properties", error))?;
        Ok(())
    }

    unsafe fn dismount_volumes_on_disk(vds: &Vds, disk_number: u32) -> Result<(), StorageError> {
        for volume in vds.volumes()? {
            let device_path = volume_guid_device_path_from_vds_object(&volume)?;
            let extents = volume_extents_from_device_path(&device_path)?;
            if !extents
                .iter()
                .any(|extent| extent.DiskNumber == disk_number)
            {
                continue;
            }
            let formatter = volume
                .cast::<IVdsVolumeMF>()
                .map_err(|error| api_error("open VDS volume dismount interface", error))?;
            let result = (Interface::vtable(&formatter).Dismount)(
                Interface::as_raw(&formatter),
                BOOL::from(true),
                BOOL::from(false),
            );
            require_exact_success("dismount target-disk volume before clean", result)?;
        }
        Ok(())
    }

    fn validate_cleaned_layout_state(
        partition_style: u32,
        partition_count: u32,
        partial_warning: bool,
    ) -> Result<(), StorageError> {
        if partition_style != windows::Win32::System::Ioctl::PARTITION_STYLE_RAW.0 as u32
            || partition_count != 0
        {
            return Err(StorageError::new(
                "verify cleaned disk layout",
                if partial_warning {
                    "VDS reported a partial clean and the retained disk is not observably RAW"
                } else {
                    "VDS reported success but the retained disk is not observably RAW"
                },
            ));
        }
        Ok(())
    }

    unsafe fn clean_and_initialize_impl(
        disk_number: u32,
        expected: Option<&DiskLayoutSnapshot>,
        style: DiskStyle,
    ) -> Result<(), StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let handle = open_disk_for_initialization(disk_number)?;
        vds.refresh()?;
        if let Some(expected) = expected {
            let actual = disk_layout_snapshot(disk_number)?;
            if &actual != expected {
                return Err(StorageError::new(
                    "verify disk before clean",
                    "physical disk identity or canonical partition layout changed",
                ));
            }
        }
        let held_before_clean = vds.find_disk(disk_number)?;
        if held_before_clean.id != disk.id || held_before_clean.size_bytes != disk.size_bytes {
            return Err(StorageError::new(
                "verify disk clean handle",
                "physical disk identity or capacity changed while opening its retained handle",
            ));
        }
        let advanced = held_before_clean
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        dismount_volumes_on_disk(&vds, disk_number)?;
        let mut raw = std::ptr::null_mut();
        let start = (Interface::vtable(&advanced).Clean)(
            Interface::as_raw(&advanced),
            BOOL::from(true),
            BOOL::from(true),
            BOOL::from(false),
            &mut raw,
        );
        let asynchronous = exact_async_interface("start VDS disk clean", start, raw)?;
        let (_, clean_warning) = wait_async_with_policy(
            "clean disk",
            &asynchronous,
            Some(VDS_ASYNCOUT_CLEAN),
            AsyncWarningPolicy::Clean,
        )?;
        vds.refresh()?;
        let retained_length = disk_length_from_handle(&handle)?;
        if retained_length != disk.size_bytes {
            return Err(StorageError::new(
                "verify cleaned disk identity",
                format!(
                    "retained physical disk reports {retained_length} bytes after clean, expected {}",
                    disk.size_bytes
                ),
            ));
        }
        let (cleaned_layout, cleaned_returned) = read_drive_layout_from_handle(&handle)?;
        if cleaned_returned < 48 {
            return Err(StorageError::new(
                "verify cleaned disk layout",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let cleaned = std::ptr::read_unaligned(
            cleaned_layout
                .as_ptr()
                .cast::<windows::Win32::System::Ioctl::DRIVE_LAYOUT_INFORMATION_EX>(),
        );
        validate_cleaned_layout_state(
            cleaned.PartitionStyle,
            cleaned.PartitionCount,
            clean_warning.is_some(),
        )?;
        // A cleaned disk is no longer in a provider pack. Confirm that VDS can now see the same
        // capacity through QueryUnallocatedDisks, but keep the retained kernel handle as the
        // authoritative identity for the following initialization IOCTL.
        let unallocated = vds.find_disk(disk_number)?;
        if unallocated.size_bytes != disk.size_bytes {
            return Err(StorageError::new(
                "verify unallocated disk identity",
                "VDS unallocated disk capacity changed after clean",
            ));
        }
        initialize_disk_ioctl(&handle, style)?;
        vds.refresh()?;
        let initialized = vds.find_disk(disk_number)?;
        let (initialized_layout, initialized_returned) = read_drive_layout_from_handle(&handle)?;
        if initialized_returned < size_of::<u32>() as u32 {
            return Err(StorageError::new(
                "verify initialized disk",
                "drive layout response does not contain PartitionStyle",
            ));
        }
        let initialized_style = disk_style_from_layout_value(std::ptr::read_unaligned(
            initialized_layout.as_ptr().cast(),
        ))?;
        if initialized.size_bytes != disk.size_bytes || initialized_style != style {
            return Err(StorageError::new(
                "verify initialized disk",
                "post-operation disk capacity or canonical partition style does not match",
            ));
        }
        Ok(())
    }

    pub unsafe fn clean_and_initialize(
        disk_number: u32,
        style: DiskStyle,
    ) -> Result<(), StorageError> {
        clean_and_initialize_impl(disk_number, None, style)
    }

    pub unsafe fn clean_and_initialize_checked(
        disk_number: u32,
        expected: &DiskLayoutSnapshot,
        style: DiskStyle,
    ) -> Result<(), StorageError> {
        clean_and_initialize_impl(disk_number, Some(expected), style)
    }

    unsafe fn verify_disk_layout_snapshot(
        disk_number: u32,
        expected: &DiskLayoutSnapshot,
        operation: &'static str,
    ) -> Result<(), StorageError> {
        let actual = disk_layout_snapshot(disk_number)?;
        if &actual != expected {
            return Err(StorageError::new(
                operation,
                "physical disk identity or canonical partition layout changed",
            ));
        }
        Ok(())
    }

    fn partition_from_snapshot(
        snapshot: &DiskLayoutSnapshot,
        offset_bytes: u64,
    ) -> Result<DiskLayoutPartitionSnapshot, StorageError> {
        let matches: Vec<_> = snapshot
            .partitions
            .iter()
            .filter(|partition| partition.offset_bytes == offset_bytes)
            .collect();
        if matches.len() != 1 || matches[0].size_bytes == 0 {
            return Err(StorageError::new(
                "bind partition from canonical snapshot",
                "snapshot does not contain exactly one non-empty partition at the requested offset",
            ));
        }
        Ok(*matches[0])
    }

    fn partition_extent_from_snapshot(
        snapshot: &DiskLayoutSnapshot,
        disk_number: u32,
        offset_bytes: u64,
    ) -> Result<VolumeIdentity, StorageError> {
        let partition = partition_from_snapshot(snapshot, offset_bytes)?;
        Ok(VolumeIdentity {
            disk_number,
            offset_bytes,
            extent_length_bytes: partition.size_bytes,
        })
    }

    fn partition_requires_advanced_disk_access_path(
        partition: DiskLayoutPartitionSnapshot,
    ) -> bool {
        matches!(
            partition.token,
            DiskLayoutPartitionToken::Gpt { partition_type, .. }
                if partition_type == guid_identity(GPT_ESP)
        )
    }

    fn same_snapshot_disk_identity(left: &DiskLayoutSnapshot, right: &DiskLayoutSnapshot) -> bool {
        left.disk_size_bytes == right.disk_size_bytes
            && left.disk == right.disk
            && same_optional_device_id(left.device_id_hash, right.device_id_hash)
    }

    fn is_mbr_container(partition: &DiskLayoutPartitionSnapshot) -> bool {
        matches!(
            partition.token,
            DiskLayoutPartitionToken::Mbr {
                partition_type: 0x05 | 0x0F | 0x85,
                ..
            }
        )
    }

    fn sorted_partitions(
        partitions: impl IntoIterator<Item = DiskLayoutPartitionSnapshot>,
    ) -> Vec<DiskLayoutPartitionSnapshot> {
        let mut partitions: Vec<_> = partitions.into_iter().collect();
        partitions.sort();
        partitions
    }

    fn same_partition_layout(left: &DiskLayoutSnapshot, right: &DiskLayoutSnapshot) -> bool {
        sorted_partitions(left.partitions.iter().copied())
            == sorted_partitions(right.partitions.iter().copied())
    }

    fn partition_end(partition: &DiskLayoutPartitionSnapshot) -> Option<u64> {
        partition.offset_bytes.checked_add(partition.size_bytes)
    }

    fn contains_partition(
        container: &DiskLayoutPartitionSnapshot,
        partition: &DiskLayoutPartitionSnapshot,
    ) -> bool {
        container.offset_bytes <= partition.offset_bytes
            && partition_end(container).is_some_and(|container_end| {
                partition_end(partition).is_some_and(|partition_end| partition_end <= container_end)
            })
    }

    fn single_container(
        partitions: &[DiskLayoutPartitionSnapshot],
    ) -> Option<Option<DiskLayoutPartitionSnapshot>> {
        let mut containers = partitions.iter().copied().filter(is_mbr_container);
        let first = containers.next();
        containers.next().is_none().then_some(first)
    }

    fn validate_create_container_delta(
        expected: &[DiskLayoutPartitionSnapshot],
        actual: &[DiskLayoutPartitionSnapshot],
        created: DiskLayoutPartitionSnapshot,
    ) -> bool {
        let Some(before) = single_container(expected) else {
            return false;
        };
        let Some(after) = single_container(actual) else {
            return false;
        };
        if is_mbr_container(&created) {
            return match (before, after) {
                (None, Some(after)) => after == created,
                _ => false,
            };
        }
        match (before, after) {
            (None, None) => true,
            (Some(before), Some(after)) if before == after => true,
            (None, Some(after)) => contains_partition(&after, &created),
            (Some(before), Some(after)) => {
                before.offset_bytes == after.offset_bytes
                    && before.token == after.token
                    && partition_end(&after)
                        .zip(partition_end(&before))
                        .is_some_and(|(after_end, before_end)| after_end >= before_end)
                    && contains_partition(&after, &created)
            }
            _ => false,
        }
    }

    fn validate_delete_container_delta(
        expected: &[DiskLayoutPartitionSnapshot],
        actual: &[DiskLayoutPartitionSnapshot],
        deleted: DiskLayoutPartitionSnapshot,
    ) -> bool {
        let Some(before) = single_container(expected) else {
            return false;
        };
        let Some(after) = single_container(actual) else {
            return false;
        };
        if is_mbr_container(&deleted) {
            return match (before, after) {
                (Some(before), None) => before == deleted,
                _ => false,
            };
        }
        match (before, after) {
            (None, None) => true,
            (Some(before), Some(after)) if before == after => true,
            (Some(before), Some(after)) => {
                let remaining_inside = actual
                    .iter()
                    .filter(|partition| !is_mbr_container(partition))
                    .filter(|partition| contains_partition(&before, partition));
                contains_partition(&before, &deleted)
                    && before.offset_bytes == after.offset_bytes
                    && before.token == after.token
                    && partition_end(&after)
                        .zip(partition_end(&before))
                        .is_some_and(|(after_end, before_end)| after_end <= before_end)
                    && remaining_inside
                        .into_iter()
                        .all(|partition| contains_partition(&after, partition))
            }
            (Some(before), None) => {
                contains_partition(&before, &deleted)
                    && actual
                        .iter()
                        .filter(|partition| !is_mbr_container(partition))
                        .all(|partition| !contains_partition(&before, partition))
            }
            _ => false,
        }
    }

    fn created_partition_delta(
        expected: &DiskLayoutSnapshot,
        actual: &DiskLayoutSnapshot,
    ) -> Option<DiskLayoutPartitionSnapshot> {
        let expected_data = sorted_partitions(
            expected
                .partitions
                .iter()
                .copied()
                .filter(|partition| !is_mbr_container(partition)),
        );
        let actual_data = sorted_partitions(
            actual
                .partitions
                .iter()
                .copied()
                .filter(|partition| !is_mbr_container(partition)),
        );
        let additions: Vec<_> = actual_data
            .iter()
            .copied()
            .filter(|partition| !expected_data.contains(partition))
            .collect();
        let removals = expected_data
            .iter()
            .filter(|partition| !actual_data.contains(partition))
            .count();
        let created = if additions.len() == 1 && removals == 0 {
            additions[0]
        } else {
            let container_additions: Vec<_> = actual
                .partitions
                .iter()
                .copied()
                .filter(|partition| {
                    is_mbr_container(partition) && !expected.partitions.contains(partition)
                })
                .collect();
            if additions.is_empty() && removals == 0 && container_additions.len() == 1 {
                container_additions[0]
            } else {
                return None;
            }
        };
        validate_create_container_delta(&expected.partitions, &actual.partitions, created)
            .then_some(created)
    }

    #[cfg(test)]
    fn partition_created_delta_matches(
        expected: &DiskLayoutSnapshot,
        actual: &DiskLayoutSnapshot,
        offset_bytes: u64,
        size_bytes: u64,
    ) -> bool {
        created_partition_delta(expected, actual).is_some_and(|created| {
            created.offset_bytes == offset_bytes && created.size_bytes == size_bytes
        })
    }

    fn partition_deleted_delta_matches(
        expected: &DiskLayoutSnapshot,
        actual: &DiskLayoutSnapshot,
        offset_bytes: u64,
    ) -> bool {
        if !same_snapshot_disk_identity(expected, actual) {
            return false;
        }
        let deleted: Vec<_> = expected
            .partitions
            .iter()
            .copied()
            .filter(|partition| partition.offset_bytes == offset_bytes)
            .collect();
        if deleted.len() != 1 {
            return false;
        }
        let deleted = deleted[0];
        let expected_data = sorted_partitions(
            expected
                .partitions
                .iter()
                .copied()
                .filter(|partition| !is_mbr_container(partition) && *partition != deleted),
        );
        let actual_data = sorted_partitions(
            actual
                .partitions
                .iter()
                .copied()
                .filter(|partition| !is_mbr_container(partition)),
        );
        let data_matches = if is_mbr_container(&deleted) {
            sorted_partitions(
                expected
                    .partitions
                    .iter()
                    .copied()
                    .filter(|partition| !is_mbr_container(partition)),
            ) == actual_data
        } else {
            expected_data == actual_data
        };
        data_matches
            && !actual.partitions.contains(&deleted)
            && validate_delete_container_delta(&expected.partitions, &actual.partitions, deleted)
    }

    fn active_flag_delta_matches(
        expected: &DiskLayoutSnapshot,
        actual: &DiskLayoutSnapshot,
        offset_bytes: u64,
        active: bool,
    ) -> bool {
        if !same_snapshot_disk_identity(expected, actual)
            || expected.partitions.len() != actual.partitions.len()
        {
            return false;
        }
        let mut desired = expected.partitions.clone();
        let targets: Vec<_> = desired
            .iter_mut()
            .filter(|partition| partition.offset_bytes == offset_bytes)
            .collect();
        if targets.len() != 1 {
            return false;
        }
        let DiskLayoutPartitionToken::Mbr { boot_indicator, .. } =
            &mut targets.into_iter().next().unwrap().token
        else {
            return false;
        };
        *boot_indicator = active;
        desired.sort();
        let mut actual_partitions = actual.partitions.clone();
        actual_partitions.sort();
        desired == actual_partitions
    }

    unsafe fn verify_partition_created(
        disk_number: u32,
        expected: &DiskLayoutSnapshot,
        selected: SelectedFreeExtent,
        expected_role: ExpectedPartitionRole,
    ) -> Result<ObservedCreatedPartition, StorageError> {
        let actual = disk_layout_snapshot(disk_number)?;
        let created = created_partition_delta(expected, &actual).ok_or_else(|| {
            StorageError::new(
                "verify created partition",
                "post-operation partition delta is not exactly one contained addition",
            )
        })?;
        if let Some(violation) =
            created_extent_selection_violation(created, selected, expected_role)
        {
            return Err(StorageError::new("verify created partition", violation));
        }
        Ok(ObservedCreatedPartition {
            created: CreatedPartition {
                offset_bytes: created.offset_bytes,
                size_bytes: created.size_bytes,
            },
            token: created.token,
        })
    }

    unsafe fn verify_partition_deleted(
        disk_number: u32,
        expected: &DiskLayoutSnapshot,
        offset_bytes: u64,
    ) -> Result<(), StorageError> {
        let actual = disk_layout_snapshot(disk_number)?;
        if !partition_deleted_delta_matches(expected, &actual, offset_bytes) {
            return Err(StorageError::new(
                "verify deleted partition",
                "post-operation disk identity or partition layout does not match the authorized change",
            ));
        }
        Ok(())
    }

    unsafe fn rollback_exact_created_partition(
        disk_number: u32,
        observed: ObservedCreatedPartition,
    ) -> Result<(), StorageError> {
        let current = disk_layout_snapshot(disk_number)?;
        let exact_matches = current
            .partitions
            .iter()
            .filter(|partition| {
                same_created_partition_after_access_path(
                    observed,
                    ObservedCreatedPartition {
                        created: CreatedPartition {
                            offset_bytes: partition.offset_bytes,
                            size_bytes: partition.size_bytes,
                        },
                        token: partition.token,
                    },
                )
            })
            .count();
        if exact_matches != 1 {
            return Err(StorageError::new(
                "roll back created partition",
                format!(
                    "expected exactly one current provider-created extent at offset {} length {}, found {exact_matches}",
                    observed.created.offset_bytes, observed.created.size_bytes
                ),
            ));
        }
        delete_partition_impl(
            disk_number,
            observed.created.offset_bytes,
            false,
            Some(&current),
        )
    }

    unsafe fn create_partition_impl(
        request: &CreatePartitionRequest,
        expected: Option<&DiskLayoutSnapshot>,
        authorization: Option<FreeExtent>,
        minimum_size: u64,
    ) -> Result<CreatedPartition, StorageError> {
        validate_create_request(request)?;
        if minimum_size == 0 || minimum_size > request.size_bytes {
            return Err(StorageError::new(
                "validate partition",
                "partition minimum capacity must be non-zero and not exceed the requested capacity",
            ));
        }
        let baseline = if let Some(expected) = expected {
            let current = disk_layout_snapshot(request.disk_number)?;
            if !same_partition_layout(expected, &current) {
                return Err(StorageError::new(
                    "verify disk before partition creation",
                    "current partition layout differs from the immediately authorized snapshot",
                ));
            }
            expected.clone()
        } else {
            disk_layout_snapshot(request.disk_number)?
        };
        if authorization.is_some() {
            if let Err(error) = update_disk_properties(request.disk_number) {
                // The caller already supplied a canonical free envelope and the real VDS create
                // plus layout readback remain authoritative. Cache invalidation improves
                // convergence but must not become another pre-create failure gate.
                log::warn!(
                    "could not invalidate disk partition cache before caller-authorized create; continuing to the real VDS operation: {error}"
                );
            }
        }
        let vds = Vds::connect()?;
        vds.refresh()?;
        let disk = vds.find_disk(request.disk_number)?;
        let canonical_style = canonical_vds_partition_style(&baseline)?;
        if disk.style != canonical_style {
            // This is provider-cache drift, not a second authorization fact. The immediately
            // captured IOCTL layout is re-read again at the actual create boundary below.
            log::warn!(
                "VDS disk partition style contradicts the current canonical drive layout: disk={}; using IOCTL PartitionStyle for create parameters",
                request.disk_number,
            );
        }
        let selected = if let Some(authorization) = authorization {
            select_caller_authorized_extent(
                &baseline,
                request.offset_bytes,
                request.size_bytes,
                authorization,
                minimum_size,
            )?
        } else {
            let raw_extents = free_extents(&disk.disk)?;
            let provider_extents = provider_default_free_extents(&disk.disk)?;
            select_free_extent(
                &raw_extents,
                &provider_extents,
                request.offset_bytes,
                request.size_bytes,
                None,
                minimum_size,
            )?
        };
        let parameters = create_parameters(
            canonical_style,
            request.kind,
            request.active,
            &request.label,
            request.preserve_gpt_metadata.as_ref(),
        )?;
        let expected_created_token = if canonical_style == VDS_PST_GPT {
            let metadata = parameters.Anonymous.GptPartInfo;
            DiskLayoutPartitionToken::Gpt {
                partition_type: guid_identity(metadata.partitionType),
                partition_id: guid_identity(metadata.partitionId),
                attributes: metadata.attributes,
            }
        } else if canonical_style == VDS_PST_MBR {
            let metadata = parameters.Anonymous.MbrPartInfo;
            DiskLayoutPartitionToken::Mbr {
                partition_type: metadata.partitionType,
                boot_indicator: metadata.bootIndicator.0 != 0,
            }
        } else {
            return Err(StorageError::new(
                "create partition",
                "canonical drive layout contains an unsupported partition style",
            ));
        };
        let expected_created_role = expected_partition_role(
            expected_created_token,
            request.preserve_gpt_metadata.is_some(),
        );
        let creator = disk
            .disk
            .cast::<IVdsCreatePartitionEx>()
            .map_err(|error| api_error("open VDS partition creator", error))?;
        let current_before_create = disk_layout_snapshot(request.disk_number)?;
        if !same_partition_layout(&baseline, &current_before_create) {
            return Err(StorageError::new(
                "verify disk before partition creation",
                "current partition layout changed at the actual VDS create boundary",
            ));
        }
        let mut selected = selected;
        // Microsoft defines ulAlign as an optional alignment request, not the disk's logical-sector
        // size. Zero delegates alignment to VDS. A real 512e Hyper-V disk rejected ulAlign=512 with
        // VDS_E_ALIGN_NOT_SECTOR_SIZE_MULTIPLE after the exact physical-disk VDS object had been
        // proven, so neither BytesPerLogicalSector nor BytesPerPhysicalSector may be repurposed as
        // ulAlign. Keep the caller's desired offset unchanged and reconcile VDS's actual extent
        // against the authorization envelope after creation.
        let alignment = VDS_PROVIDER_DEFAULT_ALIGNMENT;
        let attempt_sizes = if authorization.is_some() && canonical_style == VDS_PST_GPT {
            // ullSize remains a byte count that must be representable in complete logical sectors.
            // Query the current physical disk only for this size normalization; it does not impose
            // an additional placement/alignment request on VDS.
            let geometry = physical_disk_sector_geometry(request.disk_number)?;
            logical_sector_create_attempt_sizes(selected, geometry.logical_sector_bytes)?
        } else {
            let mut sizes = vec![selected.requested_size];
            if selected.minimum_size < selected.requested_size {
                sizes.push(selected.minimum_size);
            }
            sizes
        };
        let (asynchronous, start_warning) = {
            let mut started = None;
            let mut first_rejection = None;
            for (attempt, size_bytes) in attempt_sizes.iter().copied().enumerate() {
                log::info!(
                    "starting VDS partition creation: disk={} offset={} size={} alignment={} minimum={} authorized_start={} authorized_end={} attempt={}/{}",
                    request.disk_number,
                    selected.offset_bytes,
                    size_bytes,
                    alignment,
                    selected.minimum_size,
                    selected.authorized_start_bytes,
                    selected.authorized_end_bytes,
                    attempt + 1,
                    attempt_sizes.len(),
                );
                let mut raw = std::ptr::null_mut();
                let start = (Interface::vtable(&creator).CreatePartitionEx)(
                    Interface::as_raw(&creator),
                    selected.offset_bytes,
                    size_bytes,
                    alignment,
                    &parameters,
                    &mut raw,
                );
                if may_retry_create_after_invalid_argument(
                    start,
                    raw.is_null(),
                    attempt,
                    attempt_sizes.len(),
                ) {
                    let current_after_rejection = disk_layout_snapshot(request.disk_number)?;
                    if !same_partition_layout(&baseline, &current_after_rejection) {
                        return Err(StorageError::new(
                            "start VDS partition creation",
                            "desired-size create returned E_INVALIDARG and the canonical layout changed (partial state); refusing a smaller retry",
                        ));
                    }
                    if first_rejection.is_none() {
                        first_rejection =
                            Some(hresult_error("start initial VDS partition creation", start));
                    }
                    log::warn!(
                        "VDS rejected authorized partition size {} with E_INVALIDARG before returning an async object; canonical layout is unchanged, retrying with authorized size {}",
                        size_bytes,
                        attempt_sizes[attempt + 1],
                    );
                    continue;
                }
                selected.requested_size = size_bytes;
                started = Some(match partition_create_async_interface(start, raw) {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(first) = first_rejection {
                            return Err(StorageError::new(
                                error.operation,
                                format!(
                                    "initial authorized attempt failed: {}; later authorized attempt failed: {}",
                                    first, error
                                ),
                            ));
                        }
                        return Err(error);
                    }
                });
                break;
            }
            started.ok_or_else(|| {
                StorageError::new(
                    "start VDS partition creation",
                    "partition creation exhausted its bounded desired/minimum attempts",
                )
            })?
        };
        let waited_and_verified = (|| -> Result<(_, _), StorageError> {
            let (output, wait_warning) = wait_async_with_policy(
                "create partition",
                &asynchronous,
                Some(VDS_ASYNCOUT_CREATEPARTITION),
                AsyncWarningPolicy::CreatePartition,
            )?;
            let created = output.Anonymous.cp;
            vds.refresh()?;
            // Do not format the new object or add an access path until the provider's structural
            // change is proven to be one contained addition of at least the requested capacity.
            // VDS is authoritative and may return legal geometry that differs slightly from the
            // request; fixed alignment and exact-request equality are not validity rules.
            let actual_created = verify_partition_created(
                request.disk_number,
                &baseline,
                selected,
                expected_created_role,
            )?;
            verify_preserved_gpt_metadata(
                request.disk_number,
                actual_created.created,
                request.preserve_gpt_metadata.as_ref(),
            )?;
            if start_warning.is_some() || wait_warning.is_some() {
                // Microsoft defines VDS_S_UPDATE_BOOTFILE_FAILED as structural success with a BCD
                // update warning. The canonical layout has now proved the one authorized delta;
                // later install boot construction remains the authoritative boot step.
                log::warn!(
                    "VDS created the partition but reported VDS_S_UPDATE_BOOTFILE_FAILED; canonical partition readback succeeded"
                );
            }
            Ok((created, actual_created))
        })();
        let (created, actual_created) = match waited_and_verified {
            Ok(value) => value,
            Err(primary) => {
                return reconcile_started_partition_creation(
                    primary,
                    &baseline,
                    selected,
                    expected_created_role,
                    || disk_layout_snapshot(request.disk_number),
                    |created| rollback_exact_created_partition(request.disk_number, created),
                );
            }
        };
        let post_create = (|| -> Result<CreatedPartition, StorageError> {
            if created.volumeId != GUID::zeroed() {
                let volume = vds.find_volume_by_id(created.volumeId)?;
                if let Some(file_system) = request.file_system {
                    format_volume(
                        &volume,
                        &FormatOptions {
                            file_system,
                            label: request.label.clone(),
                            allocation_unit_size: 0,
                            quick: true,
                            force_dismount: false,
                        },
                    )?;
                }
                if let Some(letter) = request.drive_letter {
                    add_access_path(
                        &volume,
                        letter,
                        VolumeIdentity {
                            disk_number: request.disk_number,
                            offset_bytes: actual_created.created.offset_bytes,
                            extent_length_bytes: actual_created.created.size_bytes,
                        },
                    )?;
                }
            } else {
                let disk = vds.find_disk(request.disk_number)?;
                if let Some(file_system) = request.file_system {
                    format_partition(
                        &disk.disk,
                        actual_created.created.offset_bytes,
                        file_system,
                        &request.label,
                    )?;
                }
                if let Some(letter) = request.drive_letter {
                    let advanced = disk
                        .disk
                        .cast::<IVdsAdvancedDisk>()
                        .map_err(|error| api_error("open VDS advanced disk", error))?;
                    let result = (Interface::vtable(&advanced).AssignDriveLetter)(
                        Interface::as_raw(&advanced),
                        actual_created.created.offset_bytes,
                        normalize_letter(letter)? as u16,
                    );
                    require_exact_success("assign partition drive letter", result)?;
                }
            }
            vds.refresh()?;
            // Formatting and access-path assignment must not have changed partition topology.
            let readback = verify_partition_created(
                request.disk_number,
                &baseline,
                selected,
                expected_created_role,
            )?;
            if !same_created_partition_after_access_path(actual_created, readback) {
                return Err(StorageError::new(
                    "verify created partition",
                    "created extent changed while formatting or assigning its access path",
                ));
            }
            verify_preserved_gpt_metadata(
                request.disk_number,
                readback.created,
                request.preserve_gpt_metadata.as_ref(),
            )?;
            Ok(readback.created)
        })();
        require_post_create_or_rollback(post_create, || {
            rollback_exact_created_partition(request.disk_number, actual_created)
        })
    }

    pub unsafe fn create_partition(
        request: &CreatePartitionRequest,
    ) -> Result<CreatedPartition, StorageError> {
        create_partition_impl(request, None, None, request.size_bytes)
    }

    pub unsafe fn create_partition_checked(
        request: &CreatePartitionRequest,
        expected: &DiskLayoutSnapshot,
    ) -> Result<CreatedPartition, StorageError> {
        create_partition_impl(request, Some(expected), None, request.size_bytes)
    }

    pub unsafe fn create_partition_checked_in_envelope(
        request: &CreatePartitionRequest,
        authorization: FreeExtent,
        minimum_size: u64,
        expected: &DiskLayoutSnapshot,
    ) -> Result<CreatedPartition, StorageError> {
        create_partition_impl(request, Some(expected), Some(authorization), minimum_size)
    }

    fn classify_delete_partition_result(result: HRESULT) -> Result<Option<HRESULT>, StorageError> {
        if result == HRESULT(0) {
            Ok(None)
        } else if result.is_ok() {
            // DeletePartition is synchronous, but providers can still return a success warning.
            // The caller must refresh and inspect the disk before surfacing the partial state.
            Ok(Some(result))
        } else {
            Err(hresult_error("delete partition", result))
        }
    }

    unsafe fn delete_partition_impl(
        disk_number: u32,
        offset_bytes: u64,
        force_protected: bool,
        expected: Option<&DiskLayoutSnapshot>,
    ) -> Result<(), StorageError> {
        if offset_bytes == 0 {
            return Err(StorageError::new(
                "delete partition",
                "partition offset must be non-zero",
            ));
        }
        let vds = Vds::connect()?;
        vds.refresh()?;
        if let Some(expected) = expected {
            verify_disk_layout_snapshot(
                disk_number,
                expected,
                "verify disk before partition deletion",
            )?;
        }
        let disk = vds.find_disk(disk_number)?;
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        let result = (Interface::vtable(&advanced).DeletePartition)(
            Interface::as_raw(&advanced),
            offset_bytes,
            BOOL::from(false),
            BOOL::from(force_protected),
        );
        let warning = classify_delete_partition_result(result)?;
        let postcheck = vds.refresh().and_then(|_| {
            if let Some(expected) = expected {
                verify_partition_deleted(disk_number, expected, offset_bytes)
            } else {
                let actual = disk_layout_snapshot(disk_number)?;
                if actual
                    .partitions
                    .iter()
                    .any(|partition| partition.offset_bytes == offset_bytes)
                {
                    Err(StorageError::new(
                        "verify deleted partition",
                        "partition still exists after the provider returned a success warning",
                    ))
                } else {
                    Ok(())
                }
            }
        });
        if let Some(warning) = warning {
            let state = match postcheck {
                Ok(()) => "the authorized deletion is visible after refresh".to_string(),
                Err(error) => format!("post-operation state could not be confirmed: {error}"),
            };
            return Err(StorageError::new(
                "delete partition",
                format!(
                    "VDS returned success warning 0x{:08X}; the disk may already be partially changed and the operation is not reported as complete ({state})",
                    warning.0 as u32
                ),
            ));
        }
        postcheck
    }

    pub unsafe fn delete_partition(
        disk_number: u32,
        offset_bytes: u64,
        force_protected: bool,
    ) -> Result<(), StorageError> {
        delete_partition_impl(disk_number, offset_bytes, force_protected, None)
    }

    pub unsafe fn delete_partition_checked(
        disk_number: u32,
        offset_bytes: u64,
        force_protected: bool,
        expected: &DiskLayoutSnapshot,
    ) -> Result<(), StorageError> {
        delete_partition_impl(disk_number, offset_bytes, force_protected, Some(expected))
    }

    pub unsafe fn format_drive(
        drive_letter: char,
        file_system: FileSystem,
        label: &str,
    ) -> Result<(), StorageError> {
        format_drive_with_options(
            drive_letter,
            &FormatOptions {
                file_system,
                label: label.to_owned(),
                allocation_unit_size: 0,
                quick: true,
                force_dismount: false,
            },
        )
    }

    pub unsafe fn format_drive_with_options(
        drive_letter: char,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        format_drive_with_options_expected(drive_letter, None, None, options)
    }

    unsafe fn read_format_result(volume_guid: &str) -> Result<(String, String), StorageError> {
        let root = wide(&format!("{}\\", volume_guid.trim_end_matches('\\')));
        let mut label = [0_u16; 261];
        let mut file_system = [0_u16; 64];
        GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut file_system),
        )
        .map_err(|error| api_error("read formatted volume properties", error))?;
        let decode = |buffer: &[u16]| {
            String::from_utf16_lossy(buffer)
                .trim_end_matches('\0')
                .to_owned()
        };
        Ok((decode(&label), decode(&file_system)))
    }

    unsafe fn verify_format_properties(
        drive_letter: char,
        volume_guid: &str,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let (label, file_system) = read_format_result(volume_guid)?;
        if !file_system.eq_ignore_ascii_case(options.file_system.name()) {
            return Err(StorageError::new(
                "verify formatted file system",
                format!(
                    "{}: reports {file_system:?}, expected {:?}",
                    drive_letter.to_ascii_uppercase(),
                    options.file_system.name()
                ),
            ));
        }
        if !label.eq_ignore_ascii_case(&options.label) {
            return Err(StorageError::new(
                "verify formatted volume label",
                format!(
                    "{}: reports {label:?}, expected {:?}",
                    drive_letter.to_ascii_uppercase(),
                    options.label
                ),
            ));
        }
        Ok(())
    }

    unsafe fn verify_format_result(
        drive_letter: char,
        expected: VolumeIdentity,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let mut last_error = String::new();
        for _ in 0..40 {
            match volume_identity(drive_letter).and_then(|actual| {
                if !same_volume_identity(actual, expected) {
                    return Err(StorageError::new(
                        "verify formatted volume identity",
                        format!(
                            "{}: maps to disk {} offset {} length {}, expected disk {} offset {} length {}",
                            drive_letter.to_ascii_uppercase(),
                            actual.disk_number,
                            actual.offset_bytes,
                            actual.extent_length_bytes,
                            expected.disk_number,
                            expected.offset_bytes,
                            expected.extent_length_bytes,
                        ),
                    ));
                }
                let guid = volume_guid_device_path_from_drive_letter(drive_letter)?;
                verify_format_properties(drive_letter, &guid, options)?;
                let rebound = volume_identity(drive_letter)?;
                let rebound_guid = volume_guid_device_path_from_drive_letter(drive_letter)?;
                if !same_volume_identity(rebound, expected)
                    || !rebound_guid.eq_ignore_ascii_case(&guid)
                {
                    return Err(StorageError::new(
                        "verify formatted volume identity",
                        "drive letter changed while reading formatted volume properties",
                    ));
                }
                Ok(())
            }) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = error.to_string(),
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(StorageError::new(
            "verify formatted volume",
            if last_error.is_empty() {
                "formatted volume did not become queryable".to_string()
            } else {
                last_error
            },
        ))
    }

    unsafe fn verify_format_result_by_guid(
        drive_letter: char,
        expected_guid: &str,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let mut last_error = String::new();
        for _ in 0..40 {
            let result =
                volume_guid_device_path_from_drive_letter(drive_letter).and_then(|actual| {
                    if !actual.eq_ignore_ascii_case(expected_guid) {
                        return Err(StorageError::new(
                            "verify formatted volume identity",
                            format!(
                                "{}: maps to {actual}, expected {expected_guid}",
                                drive_letter.to_ascii_uppercase()
                            ),
                        ));
                    }
                    verify_format_properties(drive_letter, expected_guid, options)?;
                    let rebound = volume_guid_device_path_from_drive_letter(drive_letter)?;
                    if !rebound.eq_ignore_ascii_case(expected_guid) {
                        return Err(StorageError::new(
                            "verify formatted volume identity",
                            "drive letter changed while reading formatted volume properties",
                        ));
                    }
                    Ok(())
                });
            match result {
                Ok(()) => return Ok(()),
                Err(error) => last_error = error.to_string(),
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(StorageError::new(
            "verify formatted volume",
            if last_error.is_empty() {
                "formatted volume did not become queryable".to_string()
            } else {
                last_error
            },
        ))
    }

    unsafe fn format_drive_with_options_expected(
        drive_letter: char,
        expected: Option<VolumeIdentity>,
        stable_expected: Option<StableVolumeIdentity>,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let label = &options.label;
        if label.encode_utf16().count() > 32 || label.contains(['\0', '\r', '\n']) {
            return Err(StorageError::new(
                "format volume",
                "volume label is too long or contains a control character",
            ));
        }
        if expected.is_none() && options.force_dismount {
            return Err(StorageError::new(
                "format volume",
                "forced dismount requires a caller-supplied stable volume identity",
            ));
        }
        if let Some(stable_expected) = stable_expected {
            let actual = stable_volume_identity(drive_letter)?;
            if !same_stable_volume_identity(actual, stable_expected) {
                return Err(StorageError::new(
                    "verify stable format target identity",
                    "disk or partition identifier changed before VDS binding",
                ));
            }
        }
        let actual = expected.map(|expected| {
            stable_volume_identity(drive_letter).and_then(|binding| {
                let actual = binding.extent;
                if same_volume_identity(actual, expected) {
                    Ok(actual)
                } else {
                    Err(StorageError::new(
                        "verify format target identity",
                        format!(
                            "{}: maps to disk {} offset {} length {}, expected disk {} offset {} length {}",
                            drive_letter.to_ascii_uppercase(),
                            actual.disk_number,
                            actual.offset_bytes,
                            actual.extent_length_bytes,
                            expected.disk_number,
                            expected.offset_bytes,
                            expected.extent_length_bytes,
                        ),
                    ))
                }
            })
        }).transpose()?;
        let initial_guid = if actual.is_none() {
            Some(volume_guid_device_path_from_drive_letter(drive_letter)?)
        } else {
            None
        };
        let vds = Vds::connect()?;
        vds.refresh()?;
        let volume = vds.find_volume_by_letter(drive_letter)?;
        if let Some(actual) = actual {
            let rebound = volume_identity(drive_letter)?;
            if !same_volume_identity(rebound, actual) {
                return Err(StorageError::new(
                    "verify format target identity",
                    format!(
                        "{}: changed physical identity while opening the VDS volume",
                        drive_letter.to_ascii_uppercase()
                    ),
                ));
            }
            let object_identity = volume_identity_from_vds_object(&volume)?;
            if !same_volume_identity(object_identity, actual) {
                return Err(StorageError::new(
                    "verify VDS format object identity",
                    format!(
                        "VDS object maps to disk {} offset {} length {}, expected disk {} offset {} length {}",
                        object_identity.disk_number,
                        object_identity.offset_bytes,
                        object_identity.extent_length_bytes,
                        actual.disk_number,
                        actual.offset_bytes,
                        actual.extent_length_bytes,
                    ),
                ));
            }
        } else if let Some(initial_guid) = initial_guid.as_deref() {
            let rebound = volume_guid_device_path_from_drive_letter(drive_letter)?;
            let object_path = volume_guid_device_path_from_vds_object(&volume)?;
            let rebound_extents = volume_extents_from_device_path(&rebound)?;
            let object_extents = volume_extents_from_device_path(&object_path)?;
            if !rebound.eq_ignore_ascii_case(initial_guid)
                || !same_volume_extent_set(&rebound_extents, &object_extents)
            {
                return Err(StorageError::new(
                    "verify VDS format object identity",
                    format!(
                        "{}: drive-letter or VDS object physical extents changed while opening the target",
                        drive_letter.to_ascii_uppercase()
                    ),
                ));
            }
        }
        if let Some(stable_expected) = stable_expected {
            let actual = stable_volume_identity(drive_letter)?;
            if !same_stable_volume_identity(actual, stable_expected) {
                return Err(StorageError::new(
                    "verify stable VDS format object identity",
                    "disk or partition identifier changed while binding the VDS object",
                ));
            }
        }
        format_volume(&volume, options)?;
        vds.refresh()?;
        let result = match (actual, initial_guid.as_deref()) {
            (Some(actual), _) => verify_format_result(drive_letter, actual, options),
            (None, Some(guid)) => verify_format_result_by_guid(drive_letter, guid, options),
            (None, None) => unreachable!("one format identity is always captured"),
        };
        result?;
        if let Some(stable_expected) = stable_expected {
            let actual = stable_volume_identity(drive_letter)?;
            if !same_stable_volume_identity(actual, stable_expected) {
                return Err(StorageError::new(
                    "verify formatted stable identity",
                    "disk or partition identifier changed after format",
                ));
            }
        }
        Ok(())
    }

    pub unsafe fn format_drive_with_options_checked(
        drive_letter: char,
        expected: VolumeIdentity,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        format_drive_with_options_expected(drive_letter, Some(expected), None, options)
    }

    pub unsafe fn format_drive_with_options_stable_checked(
        drive_letter: char,
        expected: StableVolumeIdentity,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        format_drive_with_options_expected(
            drive_letter,
            Some(expected.extent),
            Some(expected),
            options,
        )
    }

    unsafe fn find_checked_volume(
        vds: &Vds,
        drive_letter: char,
        expected: VolumeIdentity,
        stable_expected: Option<StableVolumeIdentity>,
        operation: &'static str,
    ) -> Result<IVdsVolume, StorageError> {
        vds.refresh()?;
        let actual = stable_volume_identity(drive_letter)?.extent;
        if !same_volume_identity(actual, expected) {
            return Err(StorageError::new(
                operation,
                format!(
                    "{}: maps to disk {} offset {} length {}, expected disk {} offset {} length {}",
                    drive_letter.to_ascii_uppercase(),
                    actual.disk_number,
                    actual.offset_bytes,
                    actual.extent_length_bytes,
                    expected.disk_number,
                    expected.offset_bytes,
                    expected.extent_length_bytes,
                ),
            ));
        }
        let volume = vds.find_volume_by_letter(drive_letter)?;
        let rebound = stable_volume_identity(drive_letter)?.extent;
        let object_identity = volume_identity_from_vds_object(&volume)?;
        if !same_volume_identity(rebound, expected)
            || !same_volume_identity(object_identity, expected)
        {
            return Err(StorageError::new(
                operation,
                "drive letter or VDS object changed while binding the volume",
            ));
        }
        if let Some(stable_expected) = stable_expected {
            let stable_actual = stable_identity_for_extent(object_identity)?;
            if !same_stable_volume_identity(stable_actual, stable_expected) {
                return Err(StorageError::new(
                    operation,
                    "VDS object disk or partition identifier does not match the stable target",
                ));
            }
        }
        Ok(volume)
    }

    fn validate_single_simple_plex(
        properties: &[VDS_VOLUME_PLEX_PROP],
    ) -> Result<GUID, StorageError> {
        if properties.len() != 1 {
            return Err(StorageError::new(
                "bind VDS volume plex",
                format!(
                    "checked basic-volume extension requires exactly one plex, found {}",
                    properties.len()
                ),
            ));
        }
        let property = properties[0];
        if property.id == GUID::zeroed()
            || property.r#type != VDS_VPT_SIMPLE
            || property.ulNumberOfMembers != 1
            || property.ullSize == 0
        {
            return Err(StorageError::new(
                "bind VDS volume plex",
                format!(
                    "checked basic-volume extension requires one non-empty simple plex with one member; id={:?} type={} members={} size={}",
                    property.id,
                    property.r#type.0,
                    property.ulNumberOfMembers,
                    property.ullSize,
                ),
            ));
        }
        Ok(property.id)
    }

    unsafe fn single_simple_volume_plex_id(volume: &IVdsVolume) -> Result<GUID, StorageError> {
        let mut raw = std::ptr::null_mut();
        let query = (Interface::vtable(volume).QueryPlexes)(Interface::as_raw(volume), &mut raw);
        let enumerator: IEnumVdsObject =
            exact_com_interface("query VDS volume plexes", query, raw)?;
        let mut properties = Vec::new();
        for object in enum_objects(&enumerator)? {
            let plex = object
                .cast::<IVdsVolumePlex>()
                .map_err(|error| api_error("open VDS volume plex", error))?;
            let mut property = VDS_VOLUME_PLEX_PROP::default();
            let get =
                (Interface::vtable(&plex).GetProperties)(Interface::as_raw(&plex), &mut property);
            require_exact_success("read VDS volume plex properties", get)?;
            properties.push(property);
        }
        validate_single_simple_plex(&properties)
    }

    unsafe fn volume_pack(volume: &IVdsVolume) -> Result<IVdsPack, StorageError> {
        let mut raw = std::ptr::null_mut();
        let result = (Interface::vtable(volume).GetPack)(Interface::as_raw(volume), &mut raw);
        exact_com_interface("open VDS volume pack", result, raw)
    }

    struct StorageManagement {
        // Keep the COM apartment alive until the WMI proxy has been released. Rust drops fields
        // in declaration order, so the interface must be declared before the apartment guard.
        services: IWbemServices,
        _apartment: ComApartment,
    }

    #[derive(Debug)]
    struct StorageManagementPartition {
        path: BSTR,
        disk_number: u32,
        partition_number: u32,
        size_bytes: u64,
    }

    impl StorageManagement {
        /// Connect to the Windows 8+ Storage Management provider without invoking PowerShell.
        ///
        /// Microsoft requires COM initialization, process/proxy security, and the
        /// `ROOT\\Microsoft\\Windows\\Storage` namespace. `RPC_E_TOO_LATE` means another part of
        /// the process already established COM security and is therefore not a connection error.
        unsafe fn connect() -> Result<Self, StorageError> {
            let apartment = ComApartment::enter()?;
            let security = CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            );
            if let Err(error) = security {
                if error.code() != RPC_E_TOO_LATE {
                    return Err(api_error(
                        "initialize Storage Management COM security",
                        error,
                    ));
                }
            }
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| api_error("create WMI locator", error))?;
            let services = locator
                .ConnectServer(
                    &BSTR::from("ROOT\\Microsoft\\Windows\\Storage"),
                    &BSTR::new(),
                    &BSTR::new(),
                    &BSTR::new(),
                    0,
                    &BSTR::new(),
                    None,
                )
                .map_err(|error| api_error("connect Storage Management namespace", error))?;
            // RPC_C_AUTHN_DEFAULT is 0xFFFFFFFF and RPC_C_AUTHZ_NONE is zero. Microsoft documents
            // this local-proxy form so the provider can impersonate the current elevated client.
            CoSetProxyBlanket(
                &services,
                u32::MAX,
                0,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .map_err(|error| api_error("set Storage Management proxy security", error))?;
            Ok(Self {
                services,
                _apartment: apartment,
            })
        }

        unsafe fn class(&self) -> Result<IWbemClassObject, StorageError> {
            let mut class = None;
            self.services
                .GetObject(
                    &BSTR::from("MSFT_Partition"),
                    WBEM_FLAG_RETURN_WBEM_COMPLETE,
                    None,
                    Some(&mut class),
                    None,
                )
                .map_err(|error| api_error("open MSFT_Partition class", error))?;
            class.ok_or_else(|| {
                StorageError::new(
                    "open MSFT_Partition class",
                    "WMI returned a null class object",
                )
            })
        }

        unsafe fn find_partition(
            &self,
            drive_letter: char,
            expected: VolumeIdentity,
        ) -> Result<StorageManagementPartition, StorageError> {
            // MSFT_Partition does not expose an Offset property. Its documented PartitionNumber
            // is ordered by offset and may change as preceding partitions change, so it is only a
            // current-session locator: first map the authorized exact extent to the current number
            // through the canonical disk IOCTL, then require the WMI instance to agree.
            let canonical_partition_number = canonical_partition_number_for_extent(expected)?;
            let enumerator = self
                .services
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from("SELECT * FROM MSFT_Partition"),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .map_err(|error| api_error("enumerate MSFT_Partition instances", error))?;
            let mut matches = Vec::new();
            for _ in 0..512 {
                let mut objects: [Option<IWbemClassObject>; 1] = [None];
                let mut returned = 0_u32;
                let result = enumerator.Next(5_000, &mut objects, &mut returned);
                if result.is_err() {
                    return Err(hresult_error("read MSFT_Partition instance", result));
                }
                if returned == 0 {
                    break;
                }
                let object = objects[0].take().ok_or_else(|| {
                    StorageError::new(
                        "read MSFT_Partition instance",
                        "WMI reported one result but returned a null object",
                    )
                })?;
                let Some(letter) = wbem_char_property(&object, w!("DriveLetter"))? else {
                    continue;
                };
                let disk_number = wbem_u32_property(&object, w!("DiskNumber"))?;
                let partition_number = wbem_u32_property(&object, w!("PartitionNumber"))?;
                let size_bytes = wbem_u64_property(&object, w!("Size"))?;
                let path = wbem_bstr_property(&object, w!("__PATH"))?;
                if storage_management_partition_matches_current_extent(
                    letter,
                    disk_number,
                    partition_number,
                    size_bytes,
                    drive_letter,
                    expected,
                    canonical_partition_number,
                ) {
                    matches.push(StorageManagementPartition {
                        path,
                        disk_number,
                        partition_number,
                        size_bytes,
                    });
                }
            }
            match matches.len() {
                1 => Ok(matches.remove(0)),
                0 => Err(StorageError::new(
                    "bind MSFT_Partition instance",
                    "no current instance matched the drive letter and canonical disk, partition number, and exact extent length",
                )),
                count => Err(StorageError::new(
                    "bind MSFT_Partition instance",
                    format!("{count} current instances matched the same authorized volume"),
                )),
            }
        }

        unsafe fn get_supported_sizes(
            &self,
            partition: &StorageManagementPartition,
        ) -> Result<(u64, u64), StorageError> {
            let output = self.exec_method(&partition.path, "GetSupportedSize", None)?;
            require_storage_method_success("MSFT_Partition.GetSupportedSize", &output)?;
            Ok((
                wbem_u64_property(&output, w!("SizeMin"))?,
                wbem_u64_property(&output, w!("SizeMax"))?,
            ))
        }

        unsafe fn resize(
            &self,
            partition: &StorageManagementPartition,
            target_size: u64,
        ) -> Result<IWbemClassObject, StorageError> {
            let class = self.class()?;
            let mut input_signature = None;
            let mut _output_signature = None;
            class
                .GetMethod(
                    w!("Resize"),
                    0,
                    &mut input_signature,
                    &mut _output_signature,
                )
                .map_err(|error| api_error("read MSFT_Partition.Resize signature", error))?;
            let input_signature = input_signature.ok_or_else(|| {
                StorageError::new(
                    "read MSFT_Partition.Resize signature",
                    "WMI returned no input-parameter class",
                )
            })?;
            let input = input_signature
                .SpawnInstance(0)
                .map_err(|error| api_error("create MSFT_Partition.Resize parameters", error))?;
            // WMI represents CIM uint64 values as a decimal VT_BSTR. The parameter instance owns
            // a copy after Put returns; the local VARIANT/BSTR can be released immediately.
            let size = VARIANT::from(BSTR::from(target_size.to_string()));
            input
                .Put(w!("Size"), 0, &size, 0)
                .map_err(|error| api_error("set MSFT_Partition.Resize Size", error))?;
            self.exec_method(&partition.path, "Resize", Some(&input))
        }

        unsafe fn exec_method(
            &self,
            path: &BSTR,
            method: &str,
            input: Option<&IWbemClassObject>,
        ) -> Result<IWbemClassObject, StorageError> {
            let mut output = None;
            self.services
                .ExecMethod(
                    path,
                    &BSTR::from(method),
                    WBEM_FLAG_RETURN_WBEM_COMPLETE,
                    None,
                    input,
                    Some(&mut output),
                    None,
                )
                .map_err(|error| api_error("execute Storage Management method", error))?;
            output.ok_or_else(|| {
                StorageError::new(
                    "execute Storage Management method",
                    "WMI returned no output-parameter object",
                )
            })
        }
    }

    unsafe fn wbem_property(
        object: &IWbemClassObject,
        name: PCWSTR,
    ) -> Result<VARIANT, StorageError> {
        let mut value = VARIANT::default();
        object
            .Get(name, 0, &mut value, None, None)
            .map_err(|error| api_error("read Storage Management property", error))?;
        Ok(value)
    }

    unsafe fn wbem_u32_property(
        object: &IWbemClassObject,
        name: PCWSTR,
    ) -> Result<u32, StorageError> {
        let value = wbem_property(object, name)?;
        u32::try_from(&value)
            .or_else(|_| i32::try_from(&value).map(|number| number as u32))
            .or_else(|_| {
                BSTR::try_from(&value).and_then(|text| {
                    text.to_string()
                        .parse::<u32>()
                        .map_err(|_| windows::core::Error::from(E_INVALIDARG))
                })
            })
            .map_err(|error| api_error("decode Storage Management uint32 property", error))
    }

    unsafe fn wbem_u64_property(
        object: &IWbemClassObject,
        name: PCWSTR,
    ) -> Result<u64, StorageError> {
        let value = wbem_property(object, name)?;
        u64::try_from(&value)
            .or_else(|_| i64::try_from(&value).map(|number| number as u64))
            .or_else(|_| {
                BSTR::try_from(&value).and_then(|text| {
                    text.to_string()
                        .parse::<u64>()
                        .map_err(|_| windows::core::Error::from(E_INVALIDARG))
                })
            })
            .map_err(|error| api_error("decode Storage Management uint64 property", error))
    }

    unsafe fn wbem_bstr_property(
        object: &IWbemClassObject,
        name: PCWSTR,
    ) -> Result<BSTR, StorageError> {
        let value = wbem_property(object, name)?;
        BSTR::try_from(&value)
            .map_err(|error| api_error("decode Storage Management string property", error))
    }

    unsafe fn wbem_char_property(
        object: &IWbemClassObject,
        name: PCWSTR,
    ) -> Result<Option<char>, StorageError> {
        let value = wbem_property(object, name)?;
        let scalar = u16::try_from(&value).ok().or_else(|| {
            u32::try_from(&value)
                .ok()
                .and_then(|value| u16::try_from(value).ok())
        });
        if let Some(scalar) = scalar {
            return Ok(char::from_u32(u32::from(scalar)));
        }
        if let Ok(text) = BSTR::try_from(&value) {
            let text = text.to_string();
            let mut characters = text.chars();
            let first = characters.next();
            if characters.next().is_none() {
                return Ok(first);
            }
        }
        Ok(None)
    }

    unsafe fn require_storage_method_success(
        operation: &'static str,
        output: &IWbemClassObject,
    ) -> Result<(), StorageError> {
        let return_value = wbem_u32_property(output, w!("ReturnValue"))?;
        if return_value == 0 {
            return Ok(());
        }
        let extended = wbem_bstr_property(output, w!("ExtendedStatus"))
            .ok()
            .map(|value| value.to_string())
            .filter(|value| !value.is_empty())
            .map(|mut value| {
                let mut cutoff = value.len().min(4_096);
                while !value.is_char_boundary(cutoff) {
                    cutoff -= 1;
                }
                value.truncate(cutoff);
                value
            })
            .unwrap_or_else(|| "no extended status".to_owned());
        Err(StorageError::new(
            operation,
            format!("provider return code {return_value}; {extended}"),
        ))
    }

    unsafe fn storage_method_return_value(output: &IWbemClassObject) -> Result<u32, StorageError> {
        wbem_u32_property(output, w!("ReturnValue"))
    }

    unsafe fn shrink_via_storage_management(
        drive_letter: char,
        expected: VolumeIdentity,
        stable_expected: Option<StableVolumeIdentity>,
        desired_bytes: u64,
        minimum_bytes: u64,
    ) -> Result<u64, StorageError> {
        let current = stable_volume_identity(drive_letter)?.extent;
        if !same_volume_identity(current, expected) {
            return Err(StorageError::new(
                "verify Storage Management shrink target",
                "the drive letter no longer names the exact authorized volume",
            ));
        }
        let storage = StorageManagement::connect()?;
        let partition = storage.find_partition(drive_letter, expected)?;
        let target_size = expected
            .extent_length_bytes
            .checked_sub(desired_bytes)
            .filter(|target| *target > 0)
            .ok_or_else(|| {
                StorageError::new(
                    "select Storage Management shrink size",
                    "desired reclaim is not smaller than the current partition",
                )
            })?;
        let boundary = stable_volume_identity(drive_letter)?.extent;
        if !same_volume_identity(boundary, expected) {
            return Err(StorageError::new(
                "verify Storage Management resize boundary",
                "the authorized volume changed after provider discovery",
            ));
        }
        log::info!(
            "VDS shrink fallback: MSFT_Partition.Resize disk={} partition={} current_size={} target_size={} desired_reclaim={} minimum_reclaim={}",
            partition.disk_number,
            partition.partition_number,
            partition.size_bytes,
            target_size,
            desired_bytes,
            minimum_bytes,
        );
        let mut output = storage.resize(&partition, target_size)?;
        let mut return_value = storage_method_return_value(&output)?;
        if return_value == 4_097 {
            // Microsoft defines 4097 as "Size Not Supported". It is the only provider result for
            // which a supported-size query can refine the same VDS desired/minimum contract. Even
            // then, retry only after canonical readback proves the first call changed nothing.
            let unchanged = stable_volume_identity(drive_letter)?.extent;
            if !same_volume_identity(unchanged, expected) {
                return Err(StorageError::new(
                    "reconcile unsupported Storage Management resize",
                    "MSFT_Partition.Resize returned Size Not Supported but the volume extent changed; preserving partial state without retry",
                ));
            }
            let (size_min, size_max) = storage.get_supported_sizes(&partition)?;
            let retry_target = storage_management_shrink_target(
                expected.extent_length_bytes,
                desired_bytes,
                minimum_bytes,
                size_min,
                size_max,
            )?;
            if retry_target == target_size {
                require_storage_method_success("MSFT_Partition.Resize", &output)?;
                return Err(StorageError::new(
                    "MSFT_Partition.Resize",
                    "provider reported Size Not Supported but its output was later decoded as success",
                ));
            }
            let retry_boundary = stable_volume_identity(drive_letter)?.extent;
            if !same_volume_identity(retry_boundary, expected) {
                return Err(StorageError::new(
                    "verify Storage Management retry boundary",
                    "the authorized volume changed after the supported-size query",
                ));
            }
            log::info!(
                "MSFT_Partition.Resize reported Size Not Supported without changing the volume; retrying provider size_min={} size_max={} target_size={}",
                size_min,
                size_max,
                retry_target,
            );
            output = storage.resize(&partition, retry_target)?;
            return_value = storage_method_return_value(&output)?;
        }
        if return_value != 0 {
            require_storage_method_success("MSFT_Partition.Resize", &output)?;
        }
        let actual = stable_volume_identity(drive_letter)?.extent;
        let actual_reclaimed = verified_shrink_reclaimed_bytes(expected, actual, minimum_bytes)?;
        if let Some(stable_expected) = stable_expected {
            let stable_actual = stable_volume_identity(drive_letter)?;
            let device_id_conflicts = matches!(
                (stable_actual.device_id_hash, stable_expected.device_id_hash),
                (Some(actual), Some(expected)) if actual != expected
            );
            if stable_actual.disk != stable_expected.disk
                || !same_stable_partition_token(stable_actual.partition, stable_expected.partition)
                || device_id_conflicts
                || stable_actual.extent != actual
            {
                return Err(StorageError::new(
                    "verify Storage Management shrunk stable identity",
                    "disk or partition identifier conflicts after shrink",
                ));
            }
        }
        Ok(actual_reclaimed)
    }

    unsafe fn fallback_after_vds_pre_mutation_failure(
        drive_letter: char,
        expected: VolumeIdentity,
        stable_expected: Option<StableVolumeIdentity>,
        desired_bytes: u64,
        minimum_bytes: u64,
        vds_error: StorageError,
    ) -> Result<u64, StorageError> {
        log::warn!(
            "VDS shrink was unavailable before an asynchronous operation started; trying Windows Storage Management API: {vds_error}"
        );
        shrink_via_storage_management(
            drive_letter,
            expected,
            stable_expected,
            desired_bytes,
            minimum_bytes,
        )
        .map_err(|fallback_error| {
            StorageError::new(
                "shrink volume with VDS and Storage Management",
                format!("VDS failed before mutation: {vds_error}; Storage Management fallback failed: {fallback_error}"),
            )
        })
    }

    pub unsafe fn shrink_volume(
        drive_letter: char,
        desired_bytes: u64,
        minimum_bytes: u64,
    ) -> Result<u64, StorageError> {
        let expected = volume_identity(drive_letter)?;
        shrink_volume_checked(drive_letter, expected, desired_bytes, minimum_bytes)
    }

    pub unsafe fn shrink_volume_checked(
        drive_letter: char,
        expected: VolumeIdentity,
        desired_bytes: u64,
        minimum_bytes: u64,
    ) -> Result<u64, StorageError> {
        shrink_volume_expected(drive_letter, expected, None, desired_bytes, minimum_bytes)
    }

    pub unsafe fn shrink_volume_stable_checked(
        drive_letter: char,
        expected: StableVolumeIdentity,
        desired_bytes: u64,
        minimum_bytes: u64,
    ) -> Result<u64, StorageError> {
        shrink_volume_expected(
            drive_letter,
            expected.extent,
            Some(expected),
            desired_bytes,
            minimum_bytes,
        )
    }

    unsafe fn shrink_volume_expected(
        drive_letter: char,
        expected: VolumeIdentity,
        stable_expected: Option<StableVolumeIdentity>,
        desired_bytes: u64,
        minimum_bytes: u64,
    ) -> Result<u64, StorageError> {
        if desired_bytes == 0 || minimum_bytes == 0 || minimum_bytes > desired_bytes {
            return Err(StorageError::new(
                "shrink volume",
                "desired and minimum shrink sizes must be non-zero and ordered",
            ));
        }
        log::info!(
            "starting primary VDS shrink for {}: desired_reclaim={} minimum_reclaim={}",
            drive_letter.to_ascii_uppercase(),
            desired_bytes,
            minimum_bytes,
        );
        let vds = match Vds::connect() {
            Ok(vds) => vds,
            Err(error) => {
                return fallback_after_vds_pre_mutation_failure(
                    drive_letter,
                    expected,
                    stable_expected,
                    desired_bytes,
                    minimum_bytes,
                    error,
                );
            }
        };
        let volume = match find_checked_volume(
            &vds,
            drive_letter,
            expected,
            stable_expected,
            "verify shrink target",
        ) {
            Ok(volume) => volume,
            Err(error) => {
                return fallback_after_vds_pre_mutation_failure(
                    drive_letter,
                    expected,
                    stable_expected,
                    desired_bytes,
                    minimum_bytes,
                    error,
                );
            }
        };
        let shrink = match volume.cast::<IVdsVolumeShrink>() {
            Ok(shrink) => shrink,
            Err(error) => {
                return fallback_after_vds_pre_mutation_failure(
                    drive_letter,
                    expected,
                    stable_expected,
                    desired_bytes,
                    minimum_bytes,
                    api_error("open VDS volume shrink interface", error),
                );
            }
        };
        let mut raw = std::ptr::null_mut();
        let start = (Interface::vtable(&shrink).Shrink)(
            Interface::as_raw(&shrink),
            desired_bytes,
            minimum_bytes,
            &mut raw,
        );
        // Receiving a valid `IVdsAsync` is the provider-mutation boundary. Before it, VDS has not
        // accepted an asynchronous shrink and the Windows 8+ Storage Management provider may be
        // tried. After it, Wait/Refresh/readback failures can describe a partially changed volume,
        // so switching providers would create a second uncontrolled mutation attempt.
        let asynchronous = match exact_async_interface("start VDS volume shrink", start, raw) {
            Ok(asynchronous) => asynchronous,
            Err(error) => {
                return fallback_after_vds_pre_mutation_failure(
                    drive_letter,
                    expected,
                    stable_expected,
                    desired_bytes,
                    minimum_bytes,
                    error,
                );
            }
        };
        let (output, _warning) = wait_async_with_policy(
            "shrink volume",
            &asynchronous,
            Some(VDS_ASYNCOUT_SHRINKVOLUME),
            AsyncWarningPolicy::Shrink,
        )?;
        let provider_reported_reclaimed = output.Anonymous.sv.ullReclaimedBytes;
        vds.refresh()?;
        let actual = volume_identity(drive_letter)?;
        let actual_reclaimed = verified_shrink_reclaimed_bytes(expected, actual, minimum_bytes)?;
        if provider_reported_reclaimed != actual_reclaimed {
            log::debug!(
                "VDS shrink provider reported {} reclaimed bytes; current volume extent proves {} bytes",
                provider_reported_reclaimed,
                actual_reclaimed
            );
        }
        if let Some(stable_expected) = stable_expected {
            let stable_actual = stable_volume_identity(drive_letter)?;
            let device_id_conflicts = matches!(
                (stable_actual.device_id_hash, stable_expected.device_id_hash),
                (Some(actual), Some(expected)) if actual != expected
            );
            if stable_actual.disk != stable_expected.disk
                || !same_stable_partition_token(stable_actual.partition, stable_expected.partition)
                || device_id_conflicts
                || stable_actual.extent != actual
            {
                return Err(StorageError::new(
                    "verify shrunk stable identity",
                    "disk or partition identifier conflicts after shrink",
                ));
            }
        }
        log::info!(
            "primary VDS shrink completed for {}: actual_reclaim={}",
            drive_letter.to_ascii_uppercase(),
            actual_reclaimed,
        );
        Ok(actual_reclaimed)
    }

    pub unsafe fn extend_volume(
        drive_letter: char,
        disk_number: u32,
        bytes_to_add: u64,
    ) -> Result<(), StorageError> {
        let expected = volume_identity(drive_letter)?;
        if expected.disk_number != disk_number {
            return Err(StorageError::new(
                "verify extend target",
                format!(
                    "{}: maps to disk {}, not requested disk {}",
                    drive_letter.to_ascii_uppercase(),
                    expected.disk_number,
                    disk_number
                ),
            ));
        }
        extend_volume_checked(drive_letter, expected, bytes_to_add)
    }

    pub unsafe fn extend_volume_checked(
        drive_letter: char,
        expected: VolumeIdentity,
        bytes_to_add: u64,
    ) -> Result<(), StorageError> {
        extend_volume_expected(drive_letter, expected, None, bytes_to_add)
    }

    pub unsafe fn extend_volume_stable_checked(
        drive_letter: char,
        expected: StableVolumeIdentity,
        bytes_to_add: u64,
    ) -> Result<(), StorageError> {
        extend_volume_expected(drive_letter, expected.extent, Some(expected), bytes_to_add)
    }

    unsafe fn extend_volume_expected(
        drive_letter: char,
        expected: VolumeIdentity,
        stable_expected: Option<StableVolumeIdentity>,
        bytes_to_add: u64,
    ) -> Result<(), StorageError> {
        if bytes_to_add == 0 {
            return Err(StorageError::new(
                "extend volume",
                "extension size must be non-zero",
            ));
        }
        let baseline = disk_layout_snapshot(expected.disk_number)?;
        let authorized_end = canonical_adjacent_authorized_end(&baseline, expected)?;
        let expected_end = expected
            .offset_bytes
            .checked_add(expected.extent_length_bytes)
            .ok_or_else(|| StorageError::new("extend volume", "volume end overflow"))?;
        let required_end = expected_end
            .checked_add(bytes_to_add)
            .ok_or_else(|| StorageError::new("extend volume", "requested end overflow"))?;
        if required_end > authorized_end {
            return Err(StorageError::new(
                "extend volume",
                "requested extension crosses the next canonical partition or disk capacity",
            ));
        }
        if let Err(error) = update_disk_properties(expected.disk_number) {
            // This is a documented cache-convergence hint, not a substitute for the real Extend
            // result and post-operation extent readback.
            log::warn!(
                "could not invalidate disk partition cache before VDS extension; continuing to the real VDS operation: {error}"
            );
        }
        let vds = Vds::connect()?;
        // Reenumerate + Refresh updates the VDS object cache, but Microsoft also documents that
        // Refresh does not force the disk driver to reread layout. Do it after the cache hint and
        // do not turn a still-stale QueryExtents inventory into a second hard gate.
        vds.refresh()?;
        let _ = find_checked_volume(
            &vds,
            drive_letter,
            expected,
            stable_expected,
            "verify extend target",
        )?;
        let volume = find_checked_volume(
            &vds,
            drive_letter,
            expected,
            stable_expected,
            "rebind extend target",
        )?;
        // `VDS_INPUT_DISK.plexId` is required for Extend. A zero GUID does not mean "the current
        // plex"; bind the one simple plex actually returned by the already identity-checked volume
        // object. The disk object must also come from this volume's pack below.
        let plex_id = single_simple_volume_plex_id(&volume)?;
        // VDS object identifiers are provider/pack scoped. A global provider enumeration may
        // expose more than one object for the same current PhysicalDrive number; feeding a disk ID
        // from another pack to this volume makes the basic provider treat a same-disk extension as
        // an unsupported multi-disk request. Microsoft exposes IVdsVolume::GetPack and
        // IVdsPack::QueryDisks specifically for this object relationship, so bind the input disk
        // through the already checked volume's own pack.
        let pack = volume_pack(&volume)?;
        let disk = vds.find_disk_in_pack(&pack, expected.disk_number)?;
        let current_before_extend = disk_layout_snapshot(expected.disk_number)?;
        if !same_partition_layout(&baseline, &current_before_extend) {
            return Err(StorageError::new(
                "verify disk before volume extension",
                "current partition layout changed at the actual VDS extend boundary",
            ));
        }
        let input = VDS_INPUT_DISK {
            diskId: disk.id,
            ullSize: bytes_to_add,
            plexId: plex_id,
            memberIdx: 0,
        };
        log::info!(
            "starting VDS volume extension: disk={} bytes={} disk_id={:?} plex_id={:?} member_index=0 source=volume_pack",
            expected.disk_number,
            bytes_to_add,
            disk.id,
            plex_id,
        );
        let mut raw = std::ptr::null_mut();
        let start =
            (Interface::vtable(&volume).Extend)(Interface::as_raw(&volume), &input, 1, &mut raw);
        let asynchronous = exact_async_interface("start VDS volume extension", start, raw)?;
        wait_async(
            "extend volume",
            &asynchronous,
            Some(VDS_ASYNCOUT_EXTENDVOLUME),
        )?;
        vds.refresh()?;
        let actual = volume_identity(drive_letter)?;
        let actual_added = verified_extend_added_bytes(
            expected,
            actual,
            bytes_to_add,
            expected_end,
            authorized_end,
        )?;
        if actual_added != bytes_to_add {
            log::debug!(
                "VDS extend requested {} bytes; current volume extent proves {} bytes were added within the authorized provider extent",
                bytes_to_add,
                actual_added
            );
        }
        if let Some(stable_expected) = stable_expected {
            let stable_actual = stable_volume_identity(drive_letter)?;
            if !same_stable_partition_identity(stable_actual, stable_expected)
                || stable_actual.extent != actual
            {
                return Err(StorageError::new(
                    "verify extended stable identity",
                    "disk or partition identifier changed after extension",
                ));
            }
        }
        Ok(())
    }

    unsafe fn set_mbr_active_impl(
        disk_number: u32,
        offset_bytes: u64,
        active: bool,
        expected: Option<&DiskLayoutSnapshot>,
    ) -> Result<(), StorageError> {
        if offset_bytes == 0 {
            return Err(StorageError::new(
                "change active partition",
                "partition offset must be non-zero",
            ));
        }
        let vds = Vds::connect()?;
        vds.refresh()?;
        let baseline = if let Some(expected) = expected {
            verify_disk_layout_snapshot(
                disk_number,
                expected,
                "verify disk before changing active partition",
            )?;
            expected.clone()
        } else {
            disk_layout_snapshot(disk_number)?
        };
        let disk = vds.find_disk(disk_number)?;
        if canonical_vds_partition_style(&baseline)? != VDS_PST_MBR {
            return Err(StorageError::new(
                "change active partition",
                "active flags are valid only on MBR disks",
            ));
        }
        let parameters = CHANGE_ATTRIBUTES_PARAMETERS {
            style: VDS_PST_MBR,
            Anonymous: CHANGE_ATTRIBUTES_PARAMETERS_0 {
                MbrPartInfo: CHANGE_ATTRIBUTES_PARAMETERS_0_1 {
                    bootIndicator: BOOLEAN(u8::from(active)),
                },
            },
        };
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        let change = (Interface::vtable(&advanced).ChangeAttributes)(
            Interface::as_raw(&advanced),
            offset_bytes,
            &parameters,
        );
        require_exact_success("change active partition", change)?;
        vds.refresh()?;
        let actual = disk_layout_snapshot(disk_number)?;
        if !active_flag_delta_matches(&baseline, &actual, offset_bytes, active) {
            return Err(StorageError::new(
                "verify active partition",
                "post-operation disk identity or partition layout does not match the authorized active-flag change",
            ));
        }
        Ok(())
    }

    pub unsafe fn set_mbr_active(
        disk_number: u32,
        offset_bytes: u64,
        active: bool,
    ) -> Result<(), StorageError> {
        set_mbr_active_impl(disk_number, offset_bytes, active, None)
    }

    pub unsafe fn set_mbr_active_checked(
        disk_number: u32,
        offset_bytes: u64,
        active: bool,
        expected: &DiskLayoutSnapshot,
    ) -> Result<(), StorageError> {
        set_mbr_active_impl(disk_number, offset_bytes, active, Some(expected))
    }

    fn drive_letter_bit(letter: char) -> u32 {
        1_u32 << (u32::from(letter as u8) - u32::from(b'A'))
    }

    unsafe fn wait_for_drive_letter_removal(letter: char) -> Result<bool, StorageError> {
        let bit = drive_letter_bit(letter);
        for _ in 0..20 {
            let mask = GetLogicalDrives();
            if mask == 0 {
                return Err(StorageError::new(
                    "verify removed drive letter",
                    windows::core::Error::from_win32().to_string(),
                ));
            }
            if mask & bit == 0 {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(false)
    }

    fn first_dos_device_target(buffer: &[u16], length: u32) -> Result<Vec<u16>, StorageError> {
        let length = usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let end = buffer[..length]
            .iter()
            .position(|value| *value == 0)
            .ok_or_else(|| {
                StorageError::new(
                    "query drive letter DOS target",
                    "QueryDosDeviceW returned an unterminated target",
                )
            })?;
        if end == 0 {
            return Err(StorageError::new(
                "query drive letter DOS target",
                "QueryDosDeviceW returned an empty target",
            ));
        }
        let mut target = buffer[..end].to_vec();
        target.push(0);
        Ok(target)
    }

    unsafe fn query_dos_device_target(letter: char) -> Result<Vec<u16>, StorageError> {
        let device_name = wide(&format!("{letter}:"));
        let mut buffer = vec![0_u16; 32_768];
        let length = QueryDosDeviceW(PCWSTR(device_name.as_ptr()), Some(&mut buffer));
        if length == 0 {
            return Err(StorageError::new(
                "query drive letter DOS target",
                windows::core::Error::from_win32().to_string(),
            ));
        }
        first_dos_device_target(&buffer, length)
    }

    unsafe fn remove_exact_dos_device_mapping(
        letter: char,
        target: &[u16],
    ) -> Result<(), StorageError> {
        let device_name = wide(&format!("{letter}:"));
        let flags = DDD_RAW_TARGET_PATH | DDD_REMOVE_DEFINITION | DDD_EXACT_MATCH_ON_REMOVE;
        DefineDosDeviceW(flags, PCWSTR(device_name.as_ptr()), PCWSTR(target.as_ptr()))
            .map_err(|error| api_error("remove exact drive letter DOS mapping", error))
    }

    unsafe fn remove_drive_letter_via_vds(
        drive_letter: char,
        force: bool,
        expected: Option<VolumeIdentity>,
    ) -> Result<(), StorageError> {
        let letter = normalize_letter(drive_letter)?;
        let vds = Vds::connect()?;
        let expected = match expected {
            Some(expected) => expected,
            None => volume_identity(letter)?,
        };
        let volume = find_checked_volume(
            &vds,
            letter,
            expected,
            None,
            "verify drive-letter removal target",
        )?;
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume access-path interface", error))?;
        let path = wide(&format!("{letter}:\\"));
        let result = (Interface::vtable(&formatter).DeleteAccessPath)(
            Interface::as_raw(&formatter),
            PCWSTR(path.as_ptr()),
            BOOL::from(force),
        );
        require_exact_success("remove drive letter access path", result)?;
        vds.refresh()
    }

    unsafe fn remove_partition_drive_letter_via_vds(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
        expected: Option<VolumeIdentity>,
    ) -> Result<(), StorageError> {
        let letter = normalize_letter(drive_letter)?;
        let vds = Vds::connect()?;
        vds.refresh()?;
        if let Some(expected) = expected {
            let actual = volume_identity(letter)?;
            if !same_volume_identity(actual, expected) {
                return Err(StorageError::new(
                    "verify partition drive-letter removal target",
                    "drive letter no longer maps to the expected volume extent",
                ));
            }
        }
        let disk = vds.find_disk(disk_number)?;
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        let mut assigned = 0_u16;
        let query = (Interface::vtable(&advanced).GetDriveLetter)(
            Interface::as_raw(&advanced),
            offset_bytes,
            PWSTR(&mut assigned),
        );
        require_exact_success("query partition drive letter", query)?;
        if assigned != letter as u16 {
            return Err(StorageError::new(
                "verify partition drive letter",
                format!(
                    "disk {disk_number} offset {offset_bytes} owns {}:, not {letter}:",
                    char::from_u32(u32::from(assigned)).unwrap_or('?')
                ),
            ));
        }
        if let Some(expected) = expected {
            let rebound = volume_identity(letter)?;
            if !same_volume_identity(rebound, expected) {
                return Err(StorageError::new(
                    "verify partition drive-letter removal target",
                    "drive letter changed while opening the VDS disk object",
                ));
            }
        }
        let remove = (Interface::vtable(&advanced).DeleteDriveLetter)(
            Interface::as_raw(&advanced),
            offset_bytes,
            letter as u16,
        );
        require_exact_success("remove partition drive letter", remove)?;
        vds.refresh()
    }

    pub unsafe fn remove_drive_letter(drive_letter: char) -> Result<(), StorageError> {
        let letter = normalize_letter(drive_letter)?;
        let expected = volume_identity(letter)?;
        let operation = remove_drive_letter_via_vds(letter, false, Some(expected));
        if wait_for_drive_letter_removal(letter)? {
            Ok(())
        } else {
            operation?;
            Err(StorageError::new(
                "verify removed drive letter",
                format!("{letter}: remains assigned after the access-path removal completed"),
            ))
        }
    }

    pub unsafe fn remove_drive_letter_if_matches(
        drive_letter: char,
        expected: VolumeIdentity,
    ) -> Result<(), StorageError> {
        let actual = volume_identity(drive_letter)?;
        if !same_volume_identity(actual, expected) {
            return Err(StorageError::new(
                "verify temporary drive letter ownership",
                format!(
                    "{}: now maps to disk {} offset {}, expected disk {} offset {}",
                    drive_letter.to_ascii_uppercase(),
                    actual.disk_number,
                    actual.offset_bytes,
                    expected.disk_number,
                    expected.offset_bytes
                ),
            ));
        }
        let letter = normalize_letter(drive_letter)?;
        let dos_target = query_dos_device_target(letter)?;
        let partition_error = remove_partition_drive_letter_via_vds(
            expected.disk_number,
            expected.offset_bytes,
            letter,
            Some(expected),
        )
        .err();
        if wait_for_drive_letter_removal(letter)? {
            return Ok(());
        }

        // Ordinary data volumes use IVdsVolumeMF. Hidden OEM/ESP partitions are not volume
        // objects and are handled by IVdsAdvancedDisk above, as required by the VDS contract.
        let volume_error = remove_drive_letter_via_vds(letter, true, Some(expected)).err();
        if wait_for_drive_letter_removal(letter)? {
            return Ok(());
        }

        remove_exact_dos_device_mapping(letter, &dos_target).map_err(|dos_error| {
            let partition_context = partition_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    "IVdsAdvancedDisk reported success but the drive letter remained".to_string()
                });
            let volume_context = volume_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    "IVdsVolumeMF reported success but the drive letter remained".to_string()
                });
            StorageError::new(
                "remove temporary drive letter",
                format!(
                    "{partition_context}; {volume_context}; exact DOS mapping cleanup also failed: {dos_error}"
                ),
            )
        })?;
        if wait_for_drive_letter_removal(letter)? {
            Ok(())
        } else {
            Err(StorageError::new(
                "verify removed temporary drive letter",
                format!("{letter}: remains assigned after exact DOS mapping cleanup"),
            ))
        }
    }

    unsafe fn remove_partition_drive_letter_impl(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
        expected_layout: Option<&DiskLayoutSnapshot>,
    ) -> Result<(), StorageError> {
        let letter = normalize_letter(drive_letter)?;
        let Some(expected) = expected_layout else {
            remove_partition_drive_letter_via_vds(disk_number, offset_bytes, letter, None)?;
            return if wait_for_drive_letter_removal(letter)? {
                Ok(())
            } else {
                Err(StorageError::new(
                    "verify removed partition drive letter",
                    format!(
                        "{letter}: remains assigned after removing it from disk {disk_number} offset {offset_bytes}"
                    ),
                ))
            };
        };
        let expected_partition = partition_from_snapshot(expected, offset_bytes)?;
        let expected_extent = partition_extent_from_snapshot(expected, disk_number, offset_bytes)?;
        let use_advanced_disk = partition_requires_advanced_disk_access_path(expected_partition);
        verify_disk_layout_snapshot(
            disk_number,
            expected,
            "verify disk before removing partition drive letter",
        )?;
        let actual = volume_identity(letter)?;
        if !same_volume_identity(actual, expected_extent) {
            return Err(StorageError::new(
                "verify partition drive-letter removal target",
                "drive letter no longer maps to the authorized physical partition",
            ));
        }
        let vds = Vds::connect()?;
        vds.refresh()?;
        verify_disk_layout_snapshot(
            disk_number,
            expected,
            "reverify disk before removing partition drive letter",
        )?;
        let ordinary_volume = if use_advanced_disk {
            // An ESP is a hidden GPT partition and is not required to expose an IVdsVolume. Its
            // canonical snapshot role already selects IVdsAdvancedDisk, avoiding a full-machine
            // volume enumeration whose unrelated WinPE RAM disk can have no physical extent.
            None
        } else {
            find_volume_for_exact_extent(&vds, expected_extent)?
        };
        match ordinary_volume {
            Some(volume) => {
                if !same_volume_identity(volume_identity_from_vds_object(&volume)?, expected_extent)
                {
                    return Err(StorageError::new(
                        "bind partition drive-letter removal target",
                        "VDS volume object changed while binding its physical extent",
                    ));
                }
                verify_disk_layout_snapshot(
                    disk_number,
                    expected,
                    "rebind ordinary volume before removing its drive letter",
                )?;
                delete_access_path(&volume, letter, false)?;
            }
            None => {
                if !use_advanced_disk {
                    return Err(StorageError::new(
                        "bind ordinary VDS volume by physical extent",
                        "the authorized ordinary data partition is not exposed as exactly one VDS volume",
                    ));
                }
                let advanced =
                    vds.find_disk_for_hidden_partition(disk_number, expected_partition)?;
                let mut assigned = 0_u16;
                let query = (Interface::vtable(&advanced).GetDriveLetter)(
                    Interface::as_raw(&advanced),
                    offset_bytes,
                    PWSTR(&mut assigned),
                );
                require_exact_success("query hidden partition drive letter", query)?;
                if assigned != letter as u16 {
                    return Err(StorageError::new(
                        "verify hidden partition drive letter",
                        "hidden partition no longer owns the authorized drive letter",
                    ));
                }
                verify_disk_layout_snapshot(
                    disk_number,
                    expected,
                    "rebind hidden partition before removing its drive letter",
                )?;
                let result = (Interface::vtable(&advanced).DeleteDriveLetter)(
                    Interface::as_raw(&advanced),
                    offset_bytes,
                    letter as u16,
                );
                require_exact_success("remove hidden partition drive letter", result)?;
            }
        }
        vds.refresh()?;
        if !wait_for_drive_letter_removal(letter)? {
            return Err(StorageError::new(
                "verify removed partition drive letter",
                format!(
                    "{letter}: remains assigned after removing it from disk {disk_number} offset {offset_bytes}"
                ),
            ));
        }
        verify_disk_layout_snapshot(
            disk_number,
            expected,
            "verify disk after removing partition drive letter",
        )
    }

    pub unsafe fn remove_partition_drive_letter(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
    ) -> Result<(), StorageError> {
        remove_partition_drive_letter_impl(disk_number, offset_bytes, drive_letter, None)
    }

    pub unsafe fn remove_partition_drive_letter_checked(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
        expected: &DiskLayoutSnapshot,
    ) -> Result<(), StorageError> {
        remove_partition_drive_letter_impl(disk_number, offset_bytes, drive_letter, Some(expected))
    }

    fn post_assign_error_with_cleanup<D, V>(
        primary: StorageError,
        expected: VolumeIdentity,
        letter: char,
        extent_owner: Result<VolumeIdentity, StorageError>,
        advanced_owner: Result<char, StorageError>,
        delete: D,
        verify_removed: V,
    ) -> StorageError
    where
        D: FnOnce() -> Result<(), StorageError>,
        V: FnOnce() -> Result<bool, StorageError>,
    {
        let extent_proves_owner = extent_owner
            .as_ref()
            .is_ok_and(|actual| same_volume_identity(*actual, expected));
        let extent_proves_other = extent_owner
            .as_ref()
            .is_ok_and(|actual| !same_volume_identity(*actual, expected));
        let advanced_proves_owner = advanced_owner
            .as_ref()
            .is_ok_and(|actual| *actual == letter);
        let advanced_proves_other = advanced_owner
            .as_ref()
            .is_ok_and(|actual| *actual != letter);

        // A current exact extent is sufficient ownership proof even if GetDriveLetter itself is
        // unavailable. Conversely, GetDriveLetter on the retained disk object and exact byte
        // offset is sufficient when Mount Manager has not made the new letter readable. Any
        // positive contradiction means the letter may have been rebound, so cleanup must preserve
        // the live mapping rather than guessing.
        let safe_to_delete = !extent_proves_other
            && !advanced_proves_other
            && (extent_proves_owner || advanced_proves_owner);
        if !safe_to_delete {
            return StorageError::new(
                primary.operation,
                format!(
                    "{}; assignment may have committed, but cleanup was skipped because current ownership was not uniquely proven (letter extent: {}; AdvancedDisk GetDriveLetter: {})",
                    primary.detail,
                    extent_owner
                        .map(|actual| format!(
                            "disk {} offset {} length {}",
                            actual.disk_number, actual.offset_bytes, actual.extent_length_bytes
                        ))
                        .unwrap_or_else(|error| error.to_string()),
                    advanced_owner
                        .map(|actual| format!("{actual}:"))
                        .unwrap_or_else(|error| error.to_string())
                ),
            );
        }

        if let Err(cleanup) = delete() {
            return StorageError::new(
                primary.operation,
                format!(
                    "{}; exact post-assignment drive-letter cleanup failed: {}",
                    primary.detail, cleanup
                ),
            );
        }
        match verify_removed() {
            Ok(true) => StorageError::new(
                primary.operation,
                format!(
                    "{}; the committed temporary drive letter was removed from the exact target",
                    primary.detail
                ),
            ),
            Ok(false) => StorageError::new(
                primary.operation,
                format!(
                    "{}; DeleteDriveLetter returned S_OK but the exact target still owns the temporary drive letter",
                    primary.detail
                ),
            ),
            Err(cleanup) => StorageError::new(
                primary.operation,
                format!(
                    "{}; DeleteDriveLetter returned S_OK, but cleanup verification failed: {}",
                    primary.detail, cleanup
                ),
            ),
        }
    }

    unsafe fn advanced_disk_drive_letter(
        advanced: &IVdsAdvancedDisk,
        offset_bytes: u64,
        operation: &'static str,
    ) -> Result<char, StorageError> {
        let mut assigned = 0_u16;
        let result = (Interface::vtable(advanced).GetDriveLetter)(
            Interface::as_raw(advanced),
            offset_bytes,
            PWSTR(&mut assigned),
        );
        require_exact_success(operation, result)?;
        let assigned = char::from_u32(u32::from(assigned)).ok_or_else(|| {
            StorageError::new(operation, "VDS returned an invalid drive-letter code unit")
        })?;
        normalize_letter(assigned)
    }

    unsafe fn reconcile_hidden_drive_letter_assignment(
        primary: StorageError,
        advanced: &IVdsAdvancedDisk,
        expected: VolumeIdentity,
        letter: char,
    ) -> StorageError {
        let extent_owner = volume_identity(letter);
        let advanced_owner = advanced_disk_drive_letter(
            advanced,
            expected.offset_bytes,
            "reconcile hidden partition drive-letter owner",
        );
        post_assign_error_with_cleanup(
            primary,
            expected,
            letter,
            extent_owner,
            advanced_owner,
            || {
                // Re-read immediately before deletion so a concurrent change cannot turn the
                // earlier ownership proof into a stale authorization.
                let current = advanced_disk_drive_letter(
                    advanced,
                    expected.offset_bytes,
                    "reverify hidden partition drive-letter owner before cleanup",
                )?;
                if current != letter {
                    return Err(StorageError::new(
                        "reverify hidden partition drive-letter owner before cleanup",
                        "the target no longer owns the temporary drive letter",
                    ));
                }
                let result = (Interface::vtable(advanced).DeleteDriveLetter)(
                    Interface::as_raw(advanced),
                    expected.offset_bytes,
                    letter as u16,
                );
                require_exact_success("cleanup hidden partition drive letter", result)
            },
            || {
                match advanced_disk_drive_letter(
                    advanced,
                    expected.offset_bytes,
                    "verify hidden partition drive-letter cleanup",
                ) {
                    Ok(current) => Ok(current != letter),
                    Err(get_error) => match volume_identity(letter) {
                        Ok(actual) => Ok(!same_volume_identity(actual, expected)),
                        Err(extent_error) => match wait_for_drive_letter_removal(letter) {
                            Ok(true) => Ok(true),
                            Ok(false) => Err(StorageError::new(
                                "verify hidden partition drive-letter cleanup",
                                format!(
                                    "GetDriveLetter failed ({get_error}); the letter remains assigned and its physical extent is unreadable ({extent_error})"
                                ),
                            )),
                            Err(wait_error) => Err(StorageError::new(
                                "verify hidden partition drive-letter cleanup",
                                format!(
                                    "GetDriveLetter failed ({get_error}); extent readback failed ({extent_error}); assignment-mask readback failed ({wait_error})"
                                ),
                            )),
                        },
                    },
                }
            },
        )
    }

    unsafe fn assign_partition_drive_letter_impl(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
        expected: Option<&DiskLayoutSnapshot>,
    ) -> Result<(), StorageError> {
        let vds = Vds::connect()?;
        vds.refresh()?;
        if let Some(expected) = expected {
            verify_disk_layout_snapshot(
                disk_number,
                expected,
                "verify disk before assigning partition drive letter",
            )?;
        }
        let letter = normalize_letter(drive_letter)?;
        let expected_partition = expected
            .map(|snapshot| partition_from_snapshot(snapshot, offset_bytes))
            .transpose()?;
        let expected_extent = expected
            .map(|snapshot| partition_extent_from_snapshot(snapshot, disk_number, offset_bytes))
            .transpose()?;
        if let Some(expected_extent) = expected_extent {
            let use_advanced_disk =
                expected_partition.is_some_and(partition_requires_advanced_disk_access_path);
            if use_advanced_disk {
                // See the symmetric removal path: a canonical GPT ESP binds directly through the
                // disk+offset API and never depends on unrelated VDS volume objects being readable.
                let advanced = vds.find_disk_for_hidden_partition(
                    disk_number,
                    expected_partition.expect("advanced assignment has a canonical partition"),
                )?;
                verify_disk_layout_snapshot(
                    disk_number,
                    expected.expect("checked assignment has an expected snapshot"),
                    "rebind hidden partition before assigning its drive letter",
                )?;
                let result = (Interface::vtable(&advanced).AssignDriveLetter)(
                    Interface::as_raw(&advanced),
                    offset_bytes,
                    letter as u16,
                );
                require_exact_success("assign hidden partition drive letter", result)?;

                let post_commit = (|| {
                    vds.refresh()?;
                    let actual = volume_identity(letter)?;
                    if !same_volume_identity(actual, expected_extent) {
                        return Err(StorageError::new(
                            "verify assigned hidden partition drive letter",
                            "drive letter does not map to the authorized physical partition",
                        ));
                    }
                    verify_disk_layout_snapshot(
                        disk_number,
                        expected.expect("checked assignment has an expected snapshot"),
                        "verify disk after assigning hidden partition drive letter",
                    )
                })();
                return post_commit.map_err(|primary| {
                    reconcile_hidden_drive_letter_assignment(
                        primary,
                        &advanced,
                        expected_extent,
                        letter,
                    )
                });
            }

            let volume = find_volume_for_exact_extent(&vds, expected_extent)?.ok_or_else(|| {
                StorageError::new(
                    "bind ordinary VDS volume by physical extent",
                    "the authorized ordinary data partition is not exposed as exactly one VDS volume",
                )
            })?;
            if !same_volume_identity(volume_identity_from_vds_object(&volume)?, expected_extent) {
                return Err(StorageError::new(
                    "bind partition drive-letter assignment target",
                    "VDS volume object changed while binding its physical extent",
                ));
            }
            verify_disk_layout_snapshot(
                disk_number,
                expected.expect("checked assignment has an expected snapshot"),
                "rebind ordinary volume before assigning its drive letter",
            )?;
            add_access_path(&volume, letter, expected_extent)?;
        } else {
            let disk = vds.find_disk(disk_number)?;
            let advanced = disk
                .disk
                .cast::<IVdsAdvancedDisk>()
                .map_err(|error| api_error("open VDS advanced disk", error))?;
            let result = (Interface::vtable(&advanced).AssignDriveLetter)(
                Interface::as_raw(&advanced),
                offset_bytes,
                letter as u16,
            );
            require_exact_success("assign partition drive letter", result)?;
        }
        vds.refresh()?;
        let actual = volume_identity(letter)?;
        if actual.disk_number != disk_number
            || actual.offset_bytes != offset_bytes
            || expected_extent.is_some_and(|expected| !same_volume_identity(actual, expected))
        {
            return Err(StorageError::new(
                "verify assigned partition drive letter",
                "drive letter does not map to the authorized physical partition",
            ));
        }
        if let Some(expected) = expected {
            verify_disk_layout_snapshot(
                disk_number,
                expected,
                "verify disk after assigning partition drive letter",
            )?;
        }
        Ok(())
    }

    pub unsafe fn assign_partition_drive_letter(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
    ) -> Result<(), StorageError> {
        assign_partition_drive_letter_impl(disk_number, offset_bytes, drive_letter, None)
    }

    pub unsafe fn assign_partition_drive_letter_checked(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
        expected: &DiskLayoutSnapshot,
    ) -> Result<(), StorageError> {
        assign_partition_drive_letter_impl(disk_number, offset_bytes, drive_letter, Some(expected))
    }

    // VDS_ASYNC_OUTPUT owns interface pointers for some operation types. The
    // operations used here return only scalar output or no object, so reading
    // the matching union arm above does not transfer an interface reference.
    #[allow(dead_code)]
    fn _assert_no_manually_drop_leak(_: ManuallyDrop<Option<IUnknown>>) {}

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn drive_layout_partition_style_uses_only_documented_values() {
            use windows::Win32::System::Ioctl::{
                PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, PARTITION_STYLE_RAW,
            };
            assert_eq!(
                disk_style_from_layout_value(PARTITION_STYLE_MBR.0 as u32).unwrap(),
                DiskStyle::Mbr
            );
            assert_eq!(
                disk_style_from_layout_value(PARTITION_STYLE_GPT.0 as u32).unwrap(),
                DiskStyle::Gpt
            );
            assert!(disk_style_from_layout_value(PARTITION_STYLE_RAW.0 as u32).is_err());
            assert!(disk_style_from_layout_value(u32::MAX).is_err());
        }

        #[test]
        fn physical_disk_inventory_sorts_and_deduplicates_authoritative_ioctl_numbers() {
            assert_eq!(
                sort_dedup_physical_disk_numbers(vec![9, 2, 9, 0, 2]),
                vec![0, 2, 9]
            );
        }

        #[test]
        fn device_number_binding_never_aliases_optical_zero_to_physical_disk_zero() {
            use windows::Win32::Storage::FileSystem::{
                FILE_DEVICE_CD_ROM, FILE_DEVICE_DISK, FILE_DEVICE_DVD,
            };

            assert_eq!(
                validated_physical_disk_device_number(FILE_DEVICE_DISK.0, 0, "test").unwrap(),
                Some((0, "test"))
            );
            assert!(
                validated_physical_disk_device_number(FILE_DEVICE_CD_ROM.0, 0, "test")
                    .unwrap_err()
                    .to_string()
                    .contains("expected FILE_DEVICE_DISK")
            );
            assert!(validated_physical_disk_device_number(FILE_DEVICE_DVD.0, 0, "test").is_err());
            assert_eq!(
                validated_physical_disk_device_number(FILE_DEVICE_DISK.0, u32::MAX, "test")
                    .unwrap(),
                None
            );
        }

        #[test]
        fn dos_device_cleanup_uses_only_the_current_exact_mapping() {
            let buffer = [
                b'\\' as u16,
                b'D' as u16,
                b'e' as u16,
                b'v' as u16,
                b'i' as u16,
                b'c' as u16,
                b'e' as u16,
                b'1' as u16,
                0,
                b'\\' as u16,
                b'D' as u16,
                b'e' as u16,
                b'v' as u16,
                b'i' as u16,
                b'c' as u16,
                b'e' as u16,
                b'2' as u16,
                0,
                0,
            ];
            let target = first_dos_device_target(&buffer, buffer.len() as u32).unwrap();
            assert_eq!(String::from_utf16_lossy(&target), "\\Device1\0");
        }

        #[test]
        fn volume_enumeration_accepts_only_terminated_guid_roots() {
            let expected = r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\";
            let mut valid: Vec<u16> = expected.encode_utf16().chain(std::iter::once(0)).collect();
            valid.resize(128, 0);
            assert_eq!(volume_name_from_buffer(&valid).unwrap(), expected);

            let unterminated: Vec<u16> = expected.encode_utf16().collect();
            assert!(volume_name_from_buffer(&unterminated).is_err());

            let invalid: Vec<u16> = r"Z:\".encode_utf16().chain(std::iter::once(0)).collect();
            assert!(volume_name_from_buffer(&invalid).is_err());
        }

        #[test]
        fn vds_format_object_accepts_documented_volume_open_paths() {
            assert_eq!(
                vds_volume_device_path(r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\")
                    .unwrap(),
                r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}"
            );
            assert_eq!(
                vds_volume_device_path(r"\\?\GLOBALROOT\Device\HarddiskVolume12").unwrap(),
                r"\\?\GLOBALROOT\Device\HarddiskVolume12"
            );
            assert!(vds_volume_device_path(r"\\.\C:").is_err());
            assert!(vds_volume_device_path(r"C:\").is_err());
            assert!(vds_volume_device_path(r"\\?\GLOBALROOT\Device\HarddiskVolumeX").is_err());
        }

        #[test]
        fn post_assign_failure_cleans_only_a_uniquely_proven_current_owner() {
            let expected = VolumeIdentity {
                disk_number: 0,
                offset_bytes: 1_048_576,
                extent_length_bytes: 300 * 1024 * 1024,
            };
            let mut deleted = false;
            let error = post_assign_error_with_cleanup(
                StorageError::new("refresh after AssignDriveLetter", "mock refresh failure"),
                expected,
                'S',
                Err(StorageError::new("read letter extent", "not ready")),
                Ok('S'),
                || {
                    deleted = true;
                    Ok(())
                },
                || Ok(true),
            );
            assert!(
                deleted,
                "GetDriveLetter on the retained exact disk+offset proves ownership"
            );
            assert!(error.to_string().contains("mock refresh failure"));
            assert!(error.to_string().contains("was removed"));

            let rebound = VolumeIdentity {
                disk_number: 9,
                ..expected
            };
            let mut deleted = false;
            let error = post_assign_error_with_cleanup(
                StorageError::new("verify assignment", "mock extent mismatch"),
                expected,
                'S',
                Ok(rebound),
                Ok('S'),
                || {
                    deleted = true;
                    Ok(())
                },
                || Ok(true),
            );
            assert!(
                !deleted,
                "a positively rebound DOS letter must never be deleted"
            );
            assert!(error.to_string().contains("cleanup was skipped"));
        }

        #[test]
        fn post_assign_failure_preserves_primary_and_cleanup_failures() {
            let expected = VolumeIdentity {
                disk_number: 0,
                offset_bytes: 1_048_576,
                extent_length_bytes: 300 * 1024 * 1024,
            };
            let error = post_assign_error_with_cleanup(
                StorageError::new("verify assignment", "primary readback failed"),
                expected,
                'S',
                Ok(expected),
                Ok('S'),
                || {
                    Err(StorageError::new(
                        "DeleteDriveLetter",
                        "mock cleanup failure",
                    ))
                },
                || panic!("verification must not run after DeleteDriveLetter failure"),
            );
            assert!(error.to_string().contains("primary readback failed"));
            assert!(error.to_string().contains("mock cleanup failure"));

            let error = post_assign_error_with_cleanup(
                StorageError::new("verify assignment", "primary readback failed"),
                expected,
                'S',
                Ok(expected),
                Ok('S'),
                || Ok(()),
                || {
                    Err(StorageError::new(
                        "cleanup readback",
                        "mock verification failure",
                    ))
                },
            );
            assert!(error.to_string().contains("primary readback failed"));
            assert!(error.to_string().contains("mock verification failure"));
        }

        #[test]
        fn canonical_gpt_esp_bypasses_global_volume_enumeration() {
            let esp = DiskLayoutPartitionSnapshot {
                offset_bytes: 1_048_576,
                size_bytes: 300 * 1024 * 1024,
                token: DiskLayoutPartitionToken::Gpt {
                    partition_type: guid_identity(GPT_ESP),
                    partition_id: [7; 16],
                    attributes: 0,
                },
            };
            assert!(partition_requires_advanced_disk_access_path(esp));

            let basic_data = DiskLayoutPartitionSnapshot {
                token: DiskLayoutPartitionToken::Gpt {
                    partition_type: guid_identity(GPT_BASIC_DATA),
                    partition_id: [7; 16],
                    attributes: 0,
                },
                ..esp
            };
            assert!(!partition_requires_advanced_disk_access_path(basic_data));
        }

        #[test]
        fn hidden_disk_binding_accepts_multiple_exact_aliases_for_one_physical_partition() {
            let first_id = GUID::from_u128(1);
            let second_id = GUID::from_u128(2);
            assert_eq!(
                finish_hidden_disk_search(
                    vec![(first_id, "disk-0")],
                    0,
                    1_048_576,
                    3,
                    Some("X: transient object")
                )
                .unwrap(),
                "disk-0"
            );
            assert!(finish_hidden_disk_search::<&str>(
                Vec::new(),
                0,
                1_048_576,
                3,
                Some("all candidates unreadable")
            )
            .is_err());
            assert_eq!(
                finish_hidden_disk_search(
                    vec![(first_id, "first"), (first_id, "duplicate")],
                    0,
                    1_048_576,
                    0,
                    None
                )
                .unwrap(),
                "first"
            );
            assert_eq!(
                finish_hidden_disk_search(
                    vec![(first_id, "basic-provider"), (second_id, "other-alias")],
                    0,
                    1_048_576,
                    0,
                    None
                )
                .unwrap(),
                "basic-provider"
            );
        }

        #[test]
        fn hidden_disk_candidate_requires_exact_offset_gpt_and_esp_role() {
            let offset = 1_048_576;
            assert!(vds_hidden_partition_matches(
                offset,
                GPT_ESP,
                offset,
                VDS_PST_GPT,
                GPT_ESP
            ));
            assert!(!vds_hidden_partition_matches(
                offset,
                GPT_ESP,
                offset + 512,
                VDS_PST_GPT,
                GPT_ESP
            ));
            assert!(!vds_hidden_partition_matches(
                offset,
                GPT_ESP,
                offset,
                VDS_PST_MBR,
                GPT_ESP
            ));
            assert!(!vds_hidden_partition_matches(
                offset,
                GPT_ESP,
                offset,
                VDS_PST_GPT,
                GPT_BASIC_DATA
            ));
        }

        #[test]
        fn vds_and_mount_manager_names_bind_by_extent_set_not_text() {
            use windows::Win32::System::Ioctl::DISK_EXTENT;

            let first = DISK_EXTENT {
                DiskNumber: 3,
                StartingOffset: 4096,
                ExtentLength: 8192,
            };
            let second = DISK_EXTENT {
                DiskNumber: 4,
                StartingOffset: 16_384,
                ExtentLength: 32_768,
            };
            assert!(same_volume_extent_set(&[first, second], &[second, first]));
            assert!(!same_volume_extent_set(
                &[first],
                &[DISK_EXTENT {
                    ExtentLength: first.ExtentLength + 1,
                    ..first
                }]
            ));
        }

        #[test]
        fn vds_warning_policy_is_operation_specific() {
            assert!(require_exact_success("test operation", HRESULT(0)).is_ok());
            assert!(require_exact_success("test operation", HRESULT(1)).is_err());
            assert!(require_exact_success("test operation", HRESULT(0x0004_241A)).is_err());
            assert!(!validate_vds_volume_name_result(HRESULT(0)).unwrap());
            assert!(validate_vds_volume_name_result(VDS_S_PROPERTIES_INCOMPLETE_HRESULT).unwrap());
            assert!(validate_vds_volume_name_result(HRESULT(1)).is_err());
            assert_eq!(
                validate_async_result(
                    "clean disk",
                    VDS_S_DISK_PARTIALLY_CLEANED_HRESULT,
                    AsyncWarningPolicy::Clean,
                )
                .unwrap(),
                Some(VDS_S_DISK_PARTIALLY_CLEANED_HRESULT)
            );
            assert_eq!(
                validate_async_result(
                    "shrink volume",
                    VDS_S_NO_NOTIFICATION_HRESULT,
                    AsyncWarningPolicy::Shrink,
                )
                .unwrap(),
                Some(VDS_S_NO_NOTIFICATION_HRESULT)
            );
            assert_eq!(
                validate_async_result(
                    "create partition",
                    VDS_S_UPDATE_BOOTFILE_FAILED_HRESULT,
                    AsyncWarningPolicy::CreatePartition,
                )
                .unwrap(),
                Some(VDS_S_UPDATE_BOOTFILE_FAILED_HRESULT)
            );
            assert!(validate_async_result(
                "format volume",
                VDS_S_UPDATE_BOOTFILE_FAILED_HRESULT,
                AsyncWarningPolicy::Exact,
            )
            .is_err());
            assert!(validate_async_result(
                "format volume",
                VDS_S_DISK_PARTIALLY_CLEANED_HRESULT,
                AsyncWarningPolicy::Exact,
            )
            .is_err());
            assert!(validate_async_result(
                "shrink volume",
                VDS_S_DISK_PARTIALLY_CLEANED_HRESULT,
                AsyncWarningPolicy::Shrink,
            )
            .is_err());
        }

        fn mbr_partition(
            offset_bytes: u64,
            size_bytes: u64,
            partition_type: u8,
            boot_indicator: bool,
        ) -> DiskLayoutPartitionSnapshot {
            DiskLayoutPartitionSnapshot {
                offset_bytes,
                size_bytes,
                token: DiskLayoutPartitionToken::Mbr {
                    partition_type,
                    boot_indicator,
                },
            }
        }

        fn mbr_snapshot(partitions: Vec<DiskLayoutPartitionSnapshot>) -> DiskLayoutSnapshot {
            DiskLayoutSnapshot {
                disk_size_bytes: 1024 * 1024 * 1024,
                disk: StableDiskIdentity::Mbr {
                    signature: 0x1234_5678,
                },
                device_id_hash: Some([7; 32]),
                partitions,
            }
        }

        fn gpt_partition(
            offset_bytes: u64,
            size_bytes: u64,
            partition_type: [u8; 16],
            partition_id: [u8; 16],
            attributes: u64,
        ) -> DiskLayoutPartitionSnapshot {
            DiskLayoutPartitionSnapshot {
                offset_bytes,
                size_bytes,
                token: DiskLayoutPartitionToken::Gpt {
                    partition_type,
                    partition_id,
                    attributes,
                },
            }
        }

        fn gpt_snapshot(partitions: Vec<DiskLayoutPartitionSnapshot>) -> DiskLayoutSnapshot {
            DiskLayoutSnapshot {
                disk_size_bytes: 1024 * 1024 * 1024,
                disk: StableDiskIdentity::Gpt { disk_id: [9; 16] },
                device_id_hash: Some([7; 32]),
                partitions,
            }
        }

        #[test]
        fn caller_authorization_bypasses_stale_vds_free_extent_inventory() {
            let source_offset = 16 * 1024 * 1024 + 512;
            let source_size = 128 * 1024 * 1024 + 4096;
            let reclaimed_offset = source_offset + source_size;
            let reclaimed_size = 12 * 1024 * 1024 + 512;
            let snapshot = gpt_snapshot(vec![gpt_partition(
                source_offset,
                source_size,
                [1; 16],
                [2; 16],
                0,
            )]);
            let selected = select_caller_authorized_extent(
                &snapshot,
                reclaimed_offset,
                reclaimed_size,
                FreeExtent {
                    offset_bytes: reclaimed_offset,
                    length_bytes: reclaimed_size,
                },
                reclaimed_size - 512,
            )
            .expect("canonical reclaimed tail is sufficient without a VDS inventory match");

            assert_eq!(selected.offset_bytes, reclaimed_offset);
            assert_eq!(selected.requested_size, reclaimed_size);
            assert_eq!(
                selected.authorized_end_bytes,
                reclaimed_offset + reclaimed_size
            );
            assert_ne!(reclaimed_offset % (1024 * 1024), 0);
            assert_ne!(reclaimed_size % (1024 * 1024), 0);
        }

        #[test]
        fn caller_authorization_still_rejects_overlap_and_insufficient_capacity() {
            let occupied =
                gpt_partition(32 * 1024 * 1024 + 512, 8 * 1024 * 1024, [1; 16], [2; 16], 0);
            let snapshot = gpt_snapshot(vec![occupied]);
            let overlapping = FreeExtent {
                offset_bytes: occupied.offset_bytes - 512,
                length_bytes: occupied.size_bytes + 1024,
            };
            assert!(select_caller_authorized_extent(
                &snapshot,
                overlapping.offset_bytes,
                4 * 1024 * 1024,
                overlapping,
                4 * 1024 * 1024,
            )
            .is_err());

            let short = FreeExtent {
                offset_bytes: 128 * 1024 * 1024 + 512,
                length_bytes: 4 * 1024 * 1024 + 512,
            };
            assert!(select_caller_authorized_extent(
                &snapshot,
                short.offset_bytes,
                8 * 1024 * 1024,
                short,
                6 * 1024 * 1024,
            )
            .is_err());
        }

        #[test]
        fn caller_authorized_create_keeps_provider_default_alignment_and_sector_sized_lengths() {
            let canonical_gpt = gpt_snapshot(Vec::new());
            let stale_vds_style = VDS_PST_MBR;
            assert_ne!(stale_vds_style, VDS_PST_GPT);
            assert_eq!(
                canonical_vds_partition_style(&canonical_gpt).unwrap(),
                VDS_PST_GPT,
                "canonical IOCTL GPT style must win even when VDS provider inventory says MBR"
            );
            assert_eq!(VDS_PROVIDER_DEFAULT_ALIGNMENT, 0);
            let selected = SelectedFreeExtent {
                offset_bytes: 75_826_707_968,
                requested_size: 10_072_621_056,
                minimum_size: 10_072_182_906,
                authorized_start_bytes: 75_826_707_968,
                raw_offset_bytes: 75_826_707_968,
                raw_size_bytes: 10_072_621_056,
                provider_offset_bytes: 75_826_707_968,
                provider_size_bytes: 10_072_621_056,
                authorized_end_bytes: 85_899_329_024,
            };
            assert_eq!(
                logical_sector_create_attempt_sizes(selected, 512).unwrap(),
                vec![10_072_621_056, 10_072_183_296]
            );
            assert!(may_retry_create_after_invalid_argument(
                E_INVALIDARG,
                true,
                0,
                2
            ));
            assert!(!may_retry_create_after_invalid_argument(
                E_INVALIDARG,
                false,
                0,
                2
            ));
            assert!(!may_retry_create_after_invalid_argument(
                HRESULT(0),
                true,
                0,
                2
            ));
            assert!(!may_retry_create_after_invalid_argument(
                E_INVALIDARG,
                true,
                1,
                2
            ));

            let exact = SelectedFreeExtent {
                minimum_size: selected.requested_size,
                ..selected
            };
            assert_eq!(
                logical_sector_create_attempt_sizes(exact, 512).unwrap(),
                vec![selected.requested_size]
            );
            let non_sector_minimum = SelectedFreeExtent {
                requested_size: 21_695_136_630,
                minimum_size: 21_695_136_630,
                authorized_end_bytes: selected.offset_bytes + 30_000_000_000,
                ..selected
            };
            assert_eq!(
                logical_sector_create_attempt_sizes(non_sector_minimum, 512).unwrap(),
                vec![21_695_136_630, 21_695_136_768]
            );
            assert_eq!(
                logical_sector_create_attempt_sizes(
                    SelectedFreeExtent {
                        offset_bytes: selected.offset_bytes + 1,
                        authorized_start_bytes: selected.authorized_start_bytes + 1,
                        authorized_end_bytes: selected.authorized_end_bytes + 1,
                        ..selected
                    },
                    512,
                )
                .unwrap(),
                vec![10_072_621_056, 10_072_183_296]
            );
            assert!(logical_sector_create_attempt_sizes(selected, 768).is_err());
        }

        #[test]
        fn access_path_allows_only_the_documented_no_drive_letter_attribute_delta() {
            let created = CreatedPartition {
                offset_bytes: 449_839_104,
                size_bytes: 21_696_086_016,
            };
            let partition_type = guid_identity(GPT_BASIC_DATA);
            let partition_id =
                guid_identity(GUID::from_u128(0x12345678_9abc_def0_1234_56789abcdef0));
            let observed = |created, attributes| ObservedCreatedPartition {
                created,
                token: DiskLayoutPartitionToken::Gpt {
                    partition_type,
                    partition_id,
                    attributes,
                },
            };
            let before = observed(created, GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER.0);
            let after = observed(created, 0);
            assert!(same_created_partition_after_access_path(before, after));
            assert!(!same_created_partition_after_access_path(
                before,
                observed(
                    CreatedPartition {
                        size_bytes: created.size_bytes + 512,
                        ..created
                    },
                    0,
                )
            ));
            assert!(!same_created_partition_after_access_path(
                before,
                observed(created, 1)
            ));
            assert!(!same_created_partition_after_access_path(
                before,
                ObservedCreatedPartition {
                    token: DiskLayoutPartitionToken::Gpt {
                        partition_type,
                        partition_id: guid_identity(GUID::from_u128(
                            0xfedcba98_7654_3210_fedc_ba9876543210,
                        )),
                        attributes: 0,
                    },
                    ..after
                }
            ));
        }

        #[test]
        fn extend_input_uses_the_one_actual_simple_plex() {
            let id = GUID::from_u128(0x01234567_89ab_cdef_0123_456789abcdef);
            let property = VDS_VOLUME_PLEX_PROP {
                id,
                r#type: VDS_VPT_SIMPLE,
                ullSize: 75_495_357_952,
                ulNumberOfMembers: 1,
                ..Default::default()
            };
            assert_eq!(validate_single_simple_plex(&[property]).unwrap(), id);
            assert!(validate_single_simple_plex(&[]).is_err());
            assert!(validate_single_simple_plex(&[property, property]).is_err());
            assert!(validate_single_simple_plex(&[VDS_VOLUME_PLEX_PROP {
                id: GUID::zeroed(),
                ..property
            }])
            .is_err());
            assert!(validate_single_simple_plex(&[VDS_VOLUME_PLEX_PROP {
                ulNumberOfMembers: 2,
                ..property
            }])
            .is_err());
        }

        #[test]
        fn canonical_layout_authorizes_extension_to_the_next_partition_boundary() {
            let source = gpt_partition(
                16 * 1024 * 1024 + 512,
                128 * 1024 * 1024 + 4096,
                [1; 16],
                [2; 16],
                0,
            );
            let next = gpt_partition(
                source.offset_bytes + source.size_bytes + 12 * 1024 * 1024 + 512,
                32 * 1024 * 1024,
                [1; 16],
                [3; 16],
                0,
            );
            let snapshot = gpt_snapshot(vec![source, next]);
            let expected = VolumeIdentity {
                disk_number: 0,
                offset_bytes: source.offset_bytes,
                extent_length_bytes: source.size_bytes,
            };

            assert_eq!(
                canonical_adjacent_authorized_end(&snapshot, expected).unwrap(),
                next.offset_bytes
            );
            assert_ne!(
                (next.offset_bytes - source.offset_bytes - source.size_bytes) % (1024 * 1024),
                0
            );

            let wrong_length = VolumeIdentity {
                extent_length_bytes: source.size_bytes - 512,
                ..expected
            };
            assert!(canonical_adjacent_authorized_end(&snapshot, wrong_length).is_err());
        }

        fn provider_selection_for(created: DiskLayoutPartitionSnapshot) -> SelectedFreeExtent {
            SelectedFreeExtent {
                offset_bytes: created.offset_bytes - 512,
                requested_size: created.size_bytes - 4096,
                minimum_size: created.size_bytes - 4096,
                authorized_start_bytes: created.offset_bytes - 4096,
                raw_offset_bytes: created.offset_bytes - 4096,
                raw_size_bytes: created.size_bytes + 16_384,
                provider_offset_bytes: created.offset_bytes - 512,
                provider_size_bytes: created.size_bytes + 8704,
                authorized_end_bytes: created.offset_bytes + created.size_bytes + 8192,
            }
        }

        fn ordinary_role(token: DiskLayoutPartitionToken) -> ExpectedPartitionRole {
            expected_partition_role(token, false)
        }

        #[test]
        fn create_wait_error_with_one_contained_provider_delta_rolls_back_exact_extent() {
            let baseline_partition = mbr_partition(4096, 100_000_123, 7, false);
            let created = mbr_partition(200_000_321, 80_000_777, 7, false);
            let baseline = mbr_snapshot(vec![baseline_partition]);
            let current = mbr_snapshot(vec![baseline_partition, created]);
            let mut deleted = None;
            let error = reconcile_started_partition_creation(
                StorageError::new("wait create partition", "mock async failure"),
                &baseline,
                provider_selection_for(created),
                ordinary_role(created.token),
                || Ok(current),
                |exact| {
                    deleted = Some(exact);
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(
                deleted,
                Some(ObservedCreatedPartition {
                    created: CreatedPartition {
                        offset_bytes: created.offset_bytes,
                        size_bytes: created.size_bytes,
                    },
                    token: created.token,
                })
            );
            assert!(error.to_string().contains("was rolled back"));
        }

        #[test]
        fn create_refresh_and_verify_errors_use_the_same_current_layout_reconciliation() {
            let created = mbr_partition(123_456_512, 90_000_512, 7, false);
            let baseline = mbr_snapshot(Vec::new());
            for (operation, detail) in [
                ("refresh VDS service", "mock refresh failure"),
                ("verify created partition", "mock first readback failure"),
            ] {
                let current = mbr_snapshot(vec![created]);
                let mut deleted = false;
                let error = reconcile_started_partition_creation(
                    StorageError::new(operation, detail),
                    &baseline,
                    provider_selection_for(created),
                    ordinary_role(created.token),
                    || Ok(current),
                    |_| {
                        deleted = true;
                        Ok(())
                    },
                )
                .unwrap_err();
                assert!(deleted);
                assert!(error.to_string().contains(detail));
                assert!(error.to_string().contains("was rolled back"));
            }
        }

        #[test]
        fn create_error_with_unchanged_layout_returns_primary_without_deletion() {
            let baseline = mbr_snapshot(vec![mbr_partition(4096, 10_000_512, 7, false)]);
            let current = baseline.clone();
            let selected = SelectedFreeExtent {
                offset_bytes: 20_000_123,
                requested_size: 5_000_321,
                minimum_size: 5_000_321,
                authorized_start_bytes: 20_000_123,
                raw_offset_bytes: 20_000_123,
                raw_size_bytes: 10_000_000,
                provider_offset_bytes: 20_000_123,
                provider_size_bytes: 10_000_000,
                authorized_end_bytes: 30_000_123,
            };
            let mut deleted = false;
            let error = reconcile_started_partition_creation(
                StorageError::new("wait create partition", "provider refused request"),
                &baseline,
                selected,
                ordinary_role(DiskLayoutPartitionToken::Mbr {
                    partition_type: 7,
                    boot_indicator: false,
                }),
                || Ok(current),
                |_| {
                    deleted = true;
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(!deleted);
            assert_eq!(error.detail, "provider refused request");
        }

        #[test]
        fn create_error_with_extra_delta_reports_partial_state_without_guessing_a_delete() {
            let created = mbr_partition(100_000_512, 20_000_321, 7, false);
            let unrelated = mbr_partition(300_000_512, 10_000_123, 7, false);
            let baseline = mbr_snapshot(Vec::new());
            let current = mbr_snapshot(vec![created, unrelated]);
            let mut deleted = false;
            let error = reconcile_started_partition_creation(
                StorageError::new("verify created partition", "mock ambiguous layout"),
                &baseline,
                provider_selection_for(created),
                ordinary_role(created.token),
                || Ok(current),
                |_| {
                    deleted = true;
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(!deleted);
            assert!(error.to_string().contains("partial state"));
            assert!(error.to_string().contains("additional or ambiguous"));
        }

        #[test]
        fn create_error_with_layout_readback_failure_reports_partial_state_without_delete() {
            let created = mbr_partition(100_000_512, 20_000_321, 7, false);
            let baseline = mbr_snapshot(Vec::new());
            let mut deleted = false;
            let error = reconcile_started_partition_creation(
                StorageError::new("refresh VDS service", "mock refresh failure"),
                &baseline,
                provider_selection_for(created),
                ordinary_role(created.token),
                || {
                    Err(StorageError::new(
                        "snapshot disk layout",
                        "mock IOCTL failure",
                    ))
                },
                |_| {
                    deleted = true;
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(!deleted);
            assert!(error.to_string().contains("partial state"));
            assert!(error.to_string().contains("mock IOCTL failure"));
        }

        #[test]
        fn topology_delta_rejects_unrelated_concurrent_changes() {
            let first = mbr_partition(1024 * 1024, 100 * 1024 * 1024, 7, false);
            let second = mbr_partition(200 * 1024 * 1024, 100 * 1024 * 1024, 7, false);
            let created = mbr_partition(400 * 1024 * 1024, 100 * 1024 * 1024, 7, false);
            let unexpected = mbr_partition(600 * 1024 * 1024, 100 * 1024 * 1024, 7, false);
            let expected = mbr_snapshot(vec![first, second]);
            let actual = mbr_snapshot(vec![first, second, created, unexpected]);
            assert!(!partition_created_delta_matches(
                &expected,
                &actual,
                created.offset_bytes,
                created.size_bytes,
            ));

            let delete_actual = mbr_snapshot(vec![]);
            assert!(!partition_deleted_delta_matches(
                &expected,
                &delete_actual,
                first.offset_bytes,
            ));

            let mut changed_first = first;
            changed_first.token = DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: true,
            };
            let active_actual = mbr_snapshot(vec![changed_first]);
            assert!(!active_flag_delta_matches(
                &expected,
                &active_actual,
                first.offset_bytes,
                true,
            ));
        }

        #[test]
        fn topology_delta_allows_only_narrow_extended_container_resize() {
            let container = mbr_partition(100 * 1024 * 1024, 500 * 1024 * 1024, 0x0F, false);
            let logical = mbr_partition(101 * 1024 * 1024, 100 * 1024 * 1024, 7, false);
            let created = mbr_partition(601 * 1024 * 1024, 100 * 1024 * 1024, 7, false);
            let grown = mbr_partition(100 * 1024 * 1024, 601 * 1024 * 1024, 0x0F, false);
            let expected = mbr_snapshot(vec![container, logical]);
            let actual = mbr_snapshot(vec![grown, logical, created]);
            assert!(partition_created_delta_matches(
                &expected,
                &actual,
                created.offset_bytes,
                created.size_bytes,
            ));

            let shrunk = mbr_partition(100 * 1024 * 1024, 101 * 1024 * 1024, 0x0F, false);
            let delete_expected = mbr_snapshot(vec![grown, logical, created]);
            let delete_actual = mbr_snapshot(vec![shrunk, logical]);
            assert!(partition_deleted_delta_matches(
                &delete_expected,
                &delete_actual,
                created.offset_bytes,
            ));

            let moved_container = mbr_partition(99 * 1024 * 1024, 102 * 1024 * 1024, 0x0F, false);
            assert!(!partition_deleted_delta_matches(
                &delete_expected,
                &mbr_snapshot(vec![moved_container, logical]),
                created.offset_bytes,
            ));
        }

        #[test]
        fn delete_partition_success_warning_is_partial_state() {
            assert_eq!(classify_delete_partition_result(HRESULT(0)).unwrap(), None);
            assert_eq!(
                classify_delete_partition_result(HRESULT(1)).unwrap(),
                Some(HRESULT(1))
            );
            assert!(classify_delete_partition_result(HRESULT(0x8000_4005_u32 as i32)).is_err());
        }

        #[test]
        fn partial_clean_warning_requires_an_observably_raw_empty_layout() {
            use windows::Win32::System::Ioctl::{PARTITION_STYLE_GPT, PARTITION_STYLE_RAW};

            assert!(validate_cleaned_layout_state(PARTITION_STYLE_RAW.0 as u32, 0, true,).is_ok());
            assert!(validate_cleaned_layout_state(PARTITION_STYLE_GPT.0 as u32, 0, true,).is_err());
            assert!(
                validate_cleaned_layout_state(PARTITION_STYLE_RAW.0 as u32, 1, false,).is_err()
            );
        }

        #[test]
        fn sdk_control_codes_encode_the_required_access_bits() {
            use windows::Win32::Storage::FileSystem::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS;
            use windows::Win32::System::Ioctl::{
                IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                IOCTL_DISK_GET_LENGTH_INFO, IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
            };

            let access = |code: u32| (code >> 14) & 0x3;
            assert_eq!(access(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS), 0);
            assert_eq!(access(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX), 0);
            assert_eq!(access(IOCTL_DISK_GET_DRIVE_LAYOUT_EX), 0);
            assert_eq!(access(IOCTL_DISK_GET_LENGTH_INFO), 1);
            assert_eq!(access(IOCTL_DISK_SET_DRIVE_LAYOUT_EX), 3);
        }

        unsafe fn device_id_descriptor_fixture() -> Vec<u8> {
            use windows::Win32::System::Ioctl::{
                StorageIdAssocDevice, StorageIdAssocPort, StorageIdCodeSetBinary,
                StorageIdTypeVendorId, STORAGE_DEVICE_ID_DESCRIPTOR, STORAGE_IDENTIFIER,
            };

            let descriptor_offset = std::mem::offset_of!(STORAGE_DEVICE_ID_DESCRIPTOR, Identifiers);
            let value_offset = std::mem::offset_of!(STORAGE_IDENTIFIER, Identifier);
            let record_size = (value_offset + 4).next_multiple_of(8);
            let mut bytes = vec![0u8; descriptor_offset + record_size * 2];
            let descriptor = STORAGE_DEVICE_ID_DESCRIPTOR {
                Version: size_of::<STORAGE_DEVICE_ID_DESCRIPTOR>() as u32,
                Size: bytes.len() as u32,
                NumberOfIdentifiers: 2,
                Identifiers: [0],
            };
            std::ptr::write_unaligned(
                bytes.as_mut_ptr().cast::<STORAGE_DEVICE_ID_DESCRIPTOR>(),
                descriptor,
            );
            for (index, association, value) in [
                (0usize, StorageIdAssocPort, [9u8, 9, 9, 9]),
                (1usize, StorageIdAssocDevice, [1u8, 2, 3, 4]),
            ] {
                let offset = descriptor_offset + index * record_size;
                let identifier = STORAGE_IDENTIFIER {
                    CodeSet: StorageIdCodeSetBinary,
                    Type: StorageIdTypeVendorId,
                    IdentifierSize: value.len() as u16,
                    NextOffset: if index == 0 { record_size as u16 } else { 0 },
                    Association: association,
                    Identifier: [0],
                };
                std::ptr::write_unaligned(
                    bytes.as_mut_ptr().add(offset).cast::<STORAGE_IDENTIFIER>(),
                    identifier,
                );
                bytes[offset + value_offset..offset + value_offset + value.len()]
                    .copy_from_slice(&value);
            }
            bytes
        }

        #[test]
        fn device_id_parser_ignores_port_ids_and_rejects_truncation() {
            let bytes = unsafe { device_id_descriptor_fixture() };
            assert_eq!(
                unsafe { normalized_device_identifiers(&bytes).unwrap() },
                vec![(1, 1, vec![1, 2, 3, 4])]
            );
            assert!(unsafe { normalized_device_identifiers(&bytes[..bytes.len() - 1]) }.is_err());
        }

        #[test]
        fn unchecked_format_cannot_request_a_forced_dismount() {
            let error = unsafe {
                format_drive_with_options(
                    'D',
                    &FormatOptions {
                        file_system: FileSystem::Ntfs,
                        label: "Data".into(),
                        allocation_unit_size: 0,
                        quick: true,
                        force_dismount: true,
                    },
                )
            }
            .unwrap_err();
            assert!(error.to_string().contains("stable volume identity"));
        }

        #[test]
        #[ignore = "requires an explicit read-only Windows volume-enumeration integration test"]
        fn volume_guid_resolution_does_not_change_drive_letters() {
            let before = assigned_drive_letter_mask().unwrap();
            let letter = current_windows_drive_letter().unwrap();
            let identity = unsafe { volume_identity(letter).unwrap() };
            let volume_root = unsafe {
                volume_guid_path_for_partition(identity.disk_number, identity.offset_bytes).unwrap()
            };
            assert!(std::path::Path::new(&volume_root).is_dir());
            assert_eq!(assigned_drive_letter_mask().unwrap(), before);
        }

        #[test]
        fn selects_raw_provider_intersection_without_rewriting_explicit_geometry() {
            const MIB: u64 = 1024 * 1024;
            let raw = [
                VDS_DISK_EXTENT {
                    ullOffset: 513,
                    ullSize: 2 * MIB,
                    r#type: VDS_DET_FREE,
                    ..Default::default()
                },
                VDS_DISK_EXTENT {
                    ullOffset: 4 * MIB,
                    ullSize: 8 * MIB,
                    r#type: VDS_DET_FREE,
                    ..Default::default()
                },
            ];
            let provider = [
                VDS_DISK_FREE_EXTENT {
                    ullOffset: 1025,
                    ullSize: 2 * MIB - 512,
                    ..Default::default()
                },
                VDS_DISK_FREE_EXTENT {
                    ullOffset: 4 * MIB,
                    ullSize: 8 * MIB,
                    ..Default::default()
                },
            ];
            assert_eq!(
                select_free_extent(&raw, &provider, 0, 3 * MIB, None, 3 * MIB).unwrap(),
                SelectedFreeExtent {
                    offset_bytes: 4 * MIB,
                    requested_size: 3 * MIB,
                    minimum_size: 3 * MIB,
                    authorized_start_bytes: 4 * MIB,
                    raw_offset_bytes: 4 * MIB,
                    raw_size_bytes: 8 * MIB,
                    provider_offset_bytes: 4 * MIB,
                    provider_size_bytes: 8 * MIB,
                    authorized_end_bytes: 12 * MIB,
                }
            );
            let explicit_offset = 513 + 4096;
            let explicit_size = 64 * 1024 + 512;
            assert_eq!(
                select_free_extent(
                    &raw,
                    &provider,
                    explicit_offset,
                    explicit_size,
                    None,
                    explicit_size,
                )
                .unwrap(),
                SelectedFreeExtent {
                    offset_bytes: explicit_offset,
                    requested_size: explicit_size,
                    minimum_size: explicit_size,
                    authorized_start_bytes: 513,
                    raw_offset_bytes: 513,
                    raw_size_bytes: 2 * MIB,
                    provider_offset_bytes: 1025,
                    provider_size_bytes: 2 * MIB - 512,
                    authorized_end_bytes: 513 + 2 * MIB,
                },
                "sector-valid explicit geometry must not be rounded to MiB"
            );
        }

        #[test]
        fn automatic_selection_uses_the_exact_short_provider_extent() {
            let provider_start = 4096 + 512;
            let provider_size = 64 * 1024 + 512;
            let raw = [VDS_DISK_EXTENT {
                ullOffset: 4096,
                ullSize: provider_size + 512,
                r#type: VDS_DET_FREE,
                ..Default::default()
            }];
            let provider = [VDS_DISK_FREE_EXTENT {
                ullOffset: provider_start,
                ullSize: provider_size,
                ..Default::default()
            }];
            assert_eq!(
                select_free_extent(&raw, &provider, 0, 64 * 1024, None, 64 * 1024).unwrap(),
                SelectedFreeExtent {
                    offset_bytes: provider_start,
                    requested_size: 64 * 1024,
                    minimum_size: 64 * 1024,
                    authorized_start_bytes: 4096,
                    raw_offset_bytes: 4096,
                    raw_size_bytes: provider_size + 512,
                    provider_offset_bytes: provider_start,
                    provider_size_bytes: provider_size,
                    authorized_end_bytes: provider_start + provider_size,
                }
            );
        }

        #[test]
        fn provider_later_start_keeps_minimum_inside_separate_authorization_envelope() {
            let envelope = FreeExtent {
                offset_bytes: 4096,
                length_bytes: 128 * 1024,
            };
            let raw = [VDS_DISK_EXTENT {
                ullOffset: envelope.offset_bytes,
                ullSize: envelope.length_bytes,
                r#type: VDS_DET_FREE,
                ..Default::default()
            }];
            let provider = [VDS_DISK_FREE_EXTENT {
                ullOffset: envelope.offset_bytes + 512,
                ullSize: envelope.length_bytes - 512,
                ..Default::default()
            }];

            assert!(
                select_free_extent(&raw, &provider, 4096, 128 * 1024, None, 128 * 1024,).is_err()
            );
            assert_eq!(
                select_free_extent(&raw, &provider, 4096, 128 * 1024, Some(envelope), 64 * 1024,)
                    .unwrap(),
                SelectedFreeExtent {
                    offset_bytes: 4096 + 512,
                    requested_size: 128 * 1024 - 512,
                    minimum_size: 64 * 1024,
                    authorized_start_bytes: 4096,
                    raw_offset_bytes: 4096,
                    raw_size_bytes: 128 * 1024,
                    provider_offset_bytes: 4096 + 512,
                    provider_size_bytes: 128 * 1024 - 512,
                    authorized_end_bytes: 4096 + 128 * 1024,
                }
            );

            let automatic =
                select_free_extent(&raw, &provider, 0, 128 * 1024, Some(envelope), 64 * 1024)
                    .unwrap();
            let token = DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: false,
            };
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: envelope.offset_bytes,
                    size_bytes: 64 * 1024,
                    token,
                },
                automatic,
                ordinary_role(token),
            ));
            assert!(!created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: envelope.offset_bytes - 512,
                    size_bytes: 64 * 1024,
                    token,
                },
                automatic,
                ordinary_role(token),
            ));
        }

        #[test]
        fn desired_request_geometry_is_not_a_hard_boundary_inside_the_authorized_envelope() {
            const MIB: u64 = 1024 * 1024;
            // Regression from LetRecoveryPE.log: QueryFreeExtents(0) returned a provider extent at
            // 1 MiB, while the desired offset was 17,408 bytes. The old selector incorrectly used
            // desired_offset + 300 MiB (314,590,208) as an authorization end, then rejected the
            // provider's actual, legal 1 MiB + 300 MiB extent (end 315,621,376).
            let request_offset = 17_408;
            let request_size = 300 * MIB;
            let provider_offset = MIB;
            let provider_size = 127_306_563_584;
            let envelope = FreeExtent {
                offset_bytes: request_offset,
                length_bytes: provider_offset + provider_size - request_offset,
            };
            let raw = [VDS_DISK_EXTENT {
                ullOffset: envelope.offset_bytes,
                ullSize: envelope.length_bytes,
                r#type: VDS_DET_FREE,
                ..Default::default()
            }];
            let provider = [VDS_DISK_FREE_EXTENT {
                ullOffset: provider_offset,
                ullSize: provider_size,
                ..Default::default()
            }];
            let selected = select_free_extent(
                &raw,
                &provider,
                request_offset,
                request_size,
                Some(envelope),
                request_size,
            )
            .unwrap();
            assert_eq!(request_offset + request_size, 314_590_208);
            assert_eq!(provider_offset + provider_size, 127_307_612_160);
            assert_eq!(selected.offset_bytes, provider_offset);
            assert_eq!(selected.requested_size, request_size);
            assert_eq!(
                selected.authorized_end_bytes,
                provider_offset + provider_size
            );

            let token = DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: false,
            };
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: provider_offset,
                    size_bytes: request_size,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert_eq!(provider_offset + request_size, 315_621_376);
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: provider_offset + 512,
                    size_bytes: request_size + 4096,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(!created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: selected.authorized_end_bytes - request_size + 1,
                    size_bytes: request_size,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
        }

        #[test]
        fn explicit_lower_bound_and_minimum_cannot_escape_authorization_envelope() {
            let envelope = FreeExtent {
                offset_bytes: 4096,
                length_bytes: 96 * 1024,
            };
            let raw = [VDS_DISK_EXTENT {
                ullOffset: 4096,
                ullSize: 96 * 1024,
                r#type: VDS_DET_FREE,
                ..Default::default()
            }];
            let provider = [VDS_DISK_FREE_EXTENT {
                ullOffset: 4096,
                ullSize: 96 * 1024,
                ..Default::default()
            }];

            let selected =
                select_free_extent(&raw, &provider, 8192, 64 * 1024, Some(envelope), 64 * 1024)
                    .unwrap();
            assert_eq!(selected.offset_bytes, 8192);
            assert_eq!(selected.requested_size, 64 * 1024);
            let token = DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: false,
            };
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: 8192,
                    size_bytes: 64 * 1024,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: 8192 - 512,
                    size_bytes: 64 * 1024,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(!created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: envelope.offset_bytes - 512,
                    size_bytes: 64 * 1024,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(select_free_extent(&raw, &provider, 4095, 4096, Some(envelope), 4096).is_err());
            assert!(select_free_extent(
                &raw,
                &provider,
                64 * 1024,
                64 * 1024,
                Some(envelope),
                64 * 1024,
            )
            .is_err());
        }

        #[test]
        fn provider_actual_geometry_may_differ_while_remaining_contained_and_large_enough() {
            let token = DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: false,
            };
            let selected = SelectedFreeExtent {
                offset_bytes: 8192,
                requested_size: 64 * 1024,
                minimum_size: 64 * 1024,
                authorized_start_bytes: 4096,
                raw_offset_bytes: 4096,
                raw_size_bytes: 256 * 1024 - 4096,
                provider_offset_bytes: 8192,
                provider_size_bytes: 256 * 1024 - 8192,
                authorized_end_bytes: 256 * 1024,
            };
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: 8192 + 512,
                    size_bytes: 64 * 1024 + 4096,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: selected.offset_bytes - 512,
                    size_bytes: 64 * 1024 + 4096,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(!created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: selected.authorized_start_bytes - 512,
                    size_bytes: 64 * 1024 + 4096,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(!created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: 8192,
                    size_bytes: 64 * 1024 - 512,
                    token,
                },
                selected,
                ordinary_role(token),
            ));
            assert!(!created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: selected.authorized_end_bytes - 1024,
                    size_bytes: 4096,
                    token,
                },
                selected,
                ordinary_role(token),
            ));

            let provider_adjusted = SelectedFreeExtent {
                requested_size: 96 * 1024,
                minimum_size: 64 * 1024,
                ..selected
            };
            assert!(created_extent_satisfies_selection(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: 8192 + 512,
                    size_bytes: 80 * 1024,
                    token,
                },
                provider_adjusted,
                ordinary_role(token),
            ));

            let mismatch = created_extent_selection_violation(
                DiskLayoutPartitionSnapshot {
                    offset_bytes: selected.offset_bytes,
                    size_bytes: selected.minimum_size,
                    token: DiskLayoutPartitionToken::Mbr {
                        partition_type: 0x0b,
                        boot_indicator: false,
                    },
                },
                selected,
                ordinary_role(token),
            )
            .expect("wrong role must remain rejected");
            assert!(mismatch.contains("actual="));
            assert!(mismatch.contains("expected="));
        }

        #[test]
        fn ordinary_gpt_creation_uses_type_as_role_but_preserved_recreation_keeps_metadata() {
            let requested = gpt_partition(8192, 64 * 1024, [1; 16], [2; 16], 0);
            let provider_actual = gpt_partition(8192, 64 * 1024, [1; 16], [3; 16], 4);
            let selected = provider_selection_for(provider_actual);

            assert!(created_extent_satisfies_selection(
                provider_actual,
                selected,
                expected_partition_role(requested.token, false),
            ));
            assert!(!created_extent_satisfies_selection(
                provider_actual,
                selected,
                expected_partition_role(requested.token, true),
            ));
            assert!(!created_extent_satisfies_selection(
                gpt_partition(8192, 64 * 1024, [8; 16], [3; 16], 4),
                selected,
                expected_partition_role(requested.token, false),
            ));
        }

        #[test]
        fn reconciliation_rolls_back_the_observed_gpt_token_after_legal_rounding() {
            let requested = gpt_partition(8192, 64 * 1024, [1; 16], [2; 16], 0);
            let provider_actual = gpt_partition(4096, 68 * 1024, [1; 16], [3; 16], 4);
            let baseline = gpt_snapshot(Vec::new());
            let current = gpt_snapshot(vec![provider_actual]);
            let mut observed = None;

            let error = reconcile_started_partition_creation(
                StorageError::new("refresh VDS service", "mock refresh failure"),
                &baseline,
                SelectedFreeExtent {
                    offset_bytes: 8192,
                    requested_size: 64 * 1024,
                    minimum_size: 64 * 1024,
                    authorized_start_bytes: 4096,
                    raw_offset_bytes: 4096,
                    raw_size_bytes: 128 * 1024,
                    provider_offset_bytes: 8192,
                    provider_size_bytes: 124 * 1024,
                    authorized_end_bytes: 132 * 1024,
                },
                expected_partition_role(requested.token, false),
                || Ok(current),
                |actual| {
                    observed = Some(actual);
                    Ok(())
                },
            )
            .unwrap_err();

            assert_eq!(
                observed,
                Some(ObservedCreatedPartition {
                    created: CreatedPartition {
                        offset_bytes: provider_actual.offset_bytes,
                        size_bytes: provider_actual.size_bytes,
                    },
                    token: provider_actual.token,
                })
            );
            assert!(error.to_string().contains("was rolled back"));
        }

        #[test]
        fn recreated_gpt_basic_partition_preserves_identity_attributes_and_name() {
            let mut name = [0_u16; 36];
            name[..4].copy_from_slice(&[b'D' as u16, b'a' as u16, b't' as u16, b'a' as u16]);
            let metadata = GptPartitionMetadata {
                partition_id: [
                    0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8,
                ],
                attributes: 0x8000_0000_0000_0001,
                name,
            };
            let parameters = unsafe {
                create_parameters(
                    VDS_PST_GPT,
                    PartitionKind::BasicData,
                    false,
                    "",
                    Some(&metadata),
                )
            }
            .unwrap();
            let actual = unsafe { parameters.Anonymous.GptPartInfo };
            assert_eq!(actual.partitionId.data1, 0x1234_5678);
            assert_eq!(actual.partitionId.data2, 0x9abc);
            assert_eq!(actual.partitionId.data3, 0xdef0);
            assert_eq!(actual.partitionId.data4, [1, 2, 3, 4, 5, 6, 7, 8]);
            assert_eq!(actual.attributes, metadata.attributes);
            assert_eq!(actual.name, metadata.name);

            let mut renamed = metadata.clone();
            renamed.name[0] = b'X' as u16;
            assert!(preserved_gpt_metadata_violation(&metadata, &metadata).is_none());
            assert_eq!(
                preserved_gpt_metadata_violation(&renamed, &metadata).as_deref(),
                Some("recreated GPT partition name differs from the requested value")
            );
        }
    }

    #[test]
    fn shrink_readback_accepts_provider_rounding_beyond_the_requested_bytes() {
        let expected = VolumeIdentity {
            disk_number: 7,
            offset_bytes: 4096 + 512,
            extent_length_bytes: 100 * 1024 * 1024 + 512,
        };
        let minimum = 8 * 1024 * 1024 + 512;
        let actual_reclaimed = minimum + 4096;
        let actual = VolumeIdentity {
            extent_length_bytes: expected.extent_length_bytes - actual_reclaimed,
            ..expected
        };
        assert_eq!(
            verified_shrink_reclaimed_bytes(expected, actual, minimum).unwrap(),
            actual_reclaimed
        );
    }

    #[test]
    fn shrink_readback_rejects_wrong_extent_or_insufficient_actual_reclaim() {
        let expected = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 1024 * 1024 + 512,
            extent_length_bytes: 32 * 1024 * 1024 + 4096,
        };
        let minimum = 8 * 1024 * 1024 + 512;
        let insufficient = VolumeIdentity {
            extent_length_bytes: expected.extent_length_bytes - (minimum - 512),
            ..expected
        };
        assert!(verified_shrink_reclaimed_bytes(expected, insufficient, minimum).is_err());
        let wrong_start = VolumeIdentity {
            offset_bytes: expected.offset_bytes + 512,
            extent_length_bytes: expected.extent_length_bytes - minimum,
            ..expected
        };
        assert!(verified_shrink_reclaimed_bytes(expected, wrong_start, minimum).is_err());
    }
}

#[cfg(windows)]
pub fn clean_and_initialize(disk_number: u32, style: DiskStyle) -> Result<(), StorageError> {
    unsafe { platform::clean_and_initialize(disk_number, style) }
}

#[cfg(windows)]
pub fn clean_and_initialize_checked(
    disk_number: u32,
    expected: &DiskLayoutSnapshot,
    style: DiskStyle,
) -> Result<(), StorageError> {
    unsafe { platform::clean_and_initialize_checked(disk_number, expected, style) }
}

#[cfg(windows)]
pub fn create_partition(
    request: &CreatePartitionRequest,
) -> Result<CreatedPartition, StorageError> {
    unsafe { platform::create_partition(request) }
}

#[cfg(windows)]
pub fn create_partition_checked(
    request: &CreatePartitionRequest,
    expected: &DiskLayoutSnapshot,
) -> Result<CreatedPartition, StorageError> {
    unsafe { platform::create_partition_checked(request, expected) }
}

/// Create the requested capacity inside the caller's current-session canonical authorization
/// envelope, allowing a shorter remainder only when it still has at least `minimum_size` bytes.
///
/// The request offset and size are desired geometry, while `authorization` is the complete hard
/// byte range the caller authorized for this topology transaction. VDS may adjust the desired
/// offset in either direction. The immediate canonical layout must still leave the envelope
/// unoccupied, and the canonical result is accepted only when it remains inside that envelope and
/// the unchanged minimum still fits. VDS free-extent inventory is intentionally not a second gate:
/// it can remain stale after a committed Shrink even after `Refresh`.
#[cfg(windows)]
pub fn create_partition_checked_in_envelope(
    request: &CreatePartitionRequest,
    authorization: FreeExtent,
    minimum_size: u64,
    expected: &DiskLayoutSnapshot,
) -> Result<CreatedPartition, StorageError> {
    unsafe {
        platform::create_partition_checked_in_envelope(
            request,
            authorization,
            minimum_size,
            expected,
        )
    }
}

#[cfg(windows)]
pub fn delete_partition(
    disk_number: u32,
    offset_bytes: u64,
    force_protected: bool,
) -> Result<(), StorageError> {
    unsafe { platform::delete_partition(disk_number, offset_bytes, force_protected) }
}

#[cfg(windows)]
pub fn delete_partition_checked(
    disk_number: u32,
    offset_bytes: u64,
    force_protected: bool,
    expected: &DiskLayoutSnapshot,
) -> Result<(), StorageError> {
    unsafe {
        platform::delete_partition_checked(disk_number, offset_bytes, force_protected, expected)
    }
}

#[cfg(windows)]
pub fn format_drive(
    drive_letter: char,
    file_system: FileSystem,
    label: &str,
) -> Result<(), StorageError> {
    unsafe { platform::format_drive(drive_letter, file_system, label) }
}

#[cfg(windows)]
pub fn format_drive_with_options(
    drive_letter: char,
    options: &FormatOptions,
) -> Result<(), StorageError> {
    unsafe { platform::format_drive_with_options(drive_letter, options) }
}

/// Format a drive only while its complete physical identity still matches the caller's stable
/// destructive target. The result is also read back and checked for identity, file system and
/// volume label before success is returned.
#[cfg(windows)]
pub fn format_drive_with_options_checked(
    drive_letter: char,
    expected: VolumeIdentity,
    options: &FormatOptions,
) -> Result<(), StorageError> {
    unsafe { platform::format_drive_with_options_checked(drive_letter, expected, options) }
}

#[cfg(windows)]
pub fn format_drive_with_options_stable_checked(
    drive_letter: char,
    expected: StableVolumeIdentity,
    options: &FormatOptions,
) -> Result<(), StorageError> {
    unsafe { platform::format_drive_with_options_stable_checked(drive_letter, expected, options) }
}

#[cfg(windows)]
pub fn shrink_volume(
    drive_letter: char,
    desired_bytes: u64,
    minimum_bytes: u64,
) -> Result<u64, StorageError> {
    unsafe { platform::shrink_volume(drive_letter, desired_bytes, minimum_bytes) }
}

#[cfg(windows)]
pub fn shrink_volume_checked(
    drive_letter: char,
    expected: VolumeIdentity,
    desired_bytes: u64,
    minimum_bytes: u64,
) -> Result<u64, StorageError> {
    unsafe { platform::shrink_volume_checked(drive_letter, expected, desired_bytes, minimum_bytes) }
}

#[cfg(windows)]
pub fn shrink_volume_stable_checked(
    drive_letter: char,
    expected: StableVolumeIdentity,
    desired_bytes: u64,
    minimum_bytes: u64,
) -> Result<u64, StorageError> {
    unsafe {
        platform::shrink_volume_stable_checked(drive_letter, expected, desired_bytes, minimum_bytes)
    }
}

#[cfg(windows)]
pub fn extend_volume(
    drive_letter: char,
    disk_number: u32,
    bytes_to_add: u64,
) -> Result<(), StorageError> {
    unsafe { platform::extend_volume(drive_letter, disk_number, bytes_to_add) }
}

#[cfg(windows)]
pub fn extend_volume_checked(
    drive_letter: char,
    expected: VolumeIdentity,
    bytes_to_add: u64,
) -> Result<(), StorageError> {
    unsafe { platform::extend_volume_checked(drive_letter, expected, bytes_to_add) }
}

#[cfg(windows)]
pub fn extend_volume_stable_checked(
    drive_letter: char,
    expected: StableVolumeIdentity,
    bytes_to_add: u64,
) -> Result<(), StorageError> {
    unsafe { platform::extend_volume_stable_checked(drive_letter, expected, bytes_to_add) }
}

#[cfg(windows)]
pub fn set_mbr_active(
    disk_number: u32,
    offset_bytes: u64,
    active: bool,
) -> Result<(), StorageError> {
    unsafe { platform::set_mbr_active(disk_number, offset_bytes, active) }
}

#[cfg(windows)]
pub fn set_mbr_active_checked(
    disk_number: u32,
    offset_bytes: u64,
    active: bool,
    expected: &DiskLayoutSnapshot,
) -> Result<(), StorageError> {
    unsafe { platform::set_mbr_active_checked(disk_number, offset_bytes, active, expected) }
}

#[cfg(windows)]
pub fn remove_drive_letter(drive_letter: char) -> Result<(), StorageError> {
    unsafe { platform::remove_drive_letter(drive_letter) }
}

/// Remove a temporary drive letter only while it still identifies the volume that was mounted.
/// This prevents a delayed cleanup from deleting a different volume's newly reused drive letter.
#[cfg(windows)]
pub fn remove_drive_letter_if_matches(
    drive_letter: char,
    expected: VolumeIdentity,
) -> Result<(), StorageError> {
    unsafe { platform::remove_drive_letter_if_matches(drive_letter, expected) }
}

/// Remove a drive letter from an OEM/ESP/unknown partition by exact disk and offset.
///
/// This is the symmetric cleanup for `IVdsAdvancedDisk::AssignDriveLetter` and remains usable
/// when Windows does not expose the hidden partition as an `IVdsVolume` object.
#[cfg(windows)]
pub fn remove_partition_drive_letter(
    disk_number: u32,
    offset_bytes: u64,
    drive_letter: char,
) -> Result<(), StorageError> {
    unsafe { platform::remove_partition_drive_letter(disk_number, offset_bytes, drive_letter) }
}

#[cfg(windows)]
pub fn remove_partition_drive_letter_checked(
    disk_number: u32,
    offset_bytes: u64,
    drive_letter: char,
    expected: &DiskLayoutSnapshot,
) -> Result<(), StorageError> {
    unsafe {
        platform::remove_partition_drive_letter_checked(
            disk_number,
            offset_bytes,
            drive_letter,
            expected,
        )
    }
}

#[cfg(windows)]
pub fn assign_partition_drive_letter(
    disk_number: u32,
    offset_bytes: u64,
    drive_letter: char,
) -> Result<(), StorageError> {
    unsafe { platform::assign_partition_drive_letter(disk_number, offset_bytes, drive_letter) }
}

#[cfg(windows)]
pub fn assign_partition_drive_letter_checked(
    disk_number: u32,
    offset_bytes: u64,
    drive_letter: char,
    expected: &DiskLayoutSnapshot,
) -> Result<(), StorageError> {
    unsafe {
        platform::assign_partition_drive_letter_checked(
            disk_number,
            offset_bytes,
            drive_letter,
            expected,
        )
    }
}

#[cfg(windows)]
pub fn current_windows_drive_letter() -> Result<char, StorageError> {
    platform::current_windows_drive_letter()
}

#[cfg(windows)]
pub fn drive_kind(drive_letter: char) -> Result<DriveKind, StorageError> {
    platform::drive_kind(drive_letter)
}

/// Current physical disks reported by SetupAPI `GUID_DEVINTERFACE_DISK` interfaces. Disk numbers
/// are current-session locators only and must not be persisted as cross-reboot identities.
#[cfg(windows)]
pub fn present_physical_disk_interfaces() -> Result<Vec<PresentDiskInterface>, StorageError> {
    unsafe { platform::present_physical_disk_interfaces() }
}

/// Resolve the current disk number through an already-open disk-interface handle.
///
/// Inventory callers use this on the same read-only handle as their capacity, layout and device
/// descriptor IOCTLs. The number is a current-session locator only. `None` is the documented MPIO
/// physical-path sentinel and must not be replaced by parsing a symbolic path.
///
/// # Safety
///
/// `handle` must remain a valid disk-interface handle for the duration of this call.
#[cfg(windows)]
pub unsafe fn present_disk_number_from_handle(
    handle: windows::Win32::Foundation::HANDLE,
) -> Result<Option<u32>, StorageError> {
    platform::query_present_disk_device_number(handle).map(|value| value.map(|(number, _)| number))
}

#[cfg(windows)]
pub fn present_physical_disk_numbers() -> Result<Vec<u32>, StorageError> {
    unsafe { platform::present_physical_disk_numbers() }
}

#[cfg(windows)]
pub fn assigned_drive_letter_mask() -> Result<u32, StorageError> {
    platform::assigned_drive_letter_mask()
}

/// Return every currently assigned drive letter that resolves to the requested physical
/// partition. Individual inaccessible roots (for example empty optical drives) are skipped after
/// `GetLogicalDrives` has provided the authoritative assignment mask.
#[cfg(windows)]
pub fn assigned_drive_letters_for_partition(
    disk_number: u32,
    offset_bytes: u64,
) -> Result<Vec<char>, StorageError> {
    let mask = assigned_drive_letter_mask()?;
    let expected = VolumeIdentity {
        disk_number,
        offset_bytes,
        extent_length_bytes: 0,
    };
    Ok((b'C'..=b'Z')
        .filter(|letter| mask & (1_u32 << u32::from(*letter - b'A')) != 0)
        .filter_map(|letter| {
            let letter = char::from(letter);
            volume_identity(letter)
                .ok()
                .filter(|actual| same_physical_partition(*actual, expected))
                .map(|_| letter)
        })
        .collect())
}

#[cfg(not(windows))]
pub fn assigned_drive_letter_mask() -> Result<u32, StorageError> {
    Err(StorageError::new(
        "enumerate assigned drive letters",
        "Windows storage APIs are unavailable",
    ))
}

#[cfg(windows)]
pub fn volume_identity(drive_letter: char) -> Result<VolumeIdentity, StorageError> {
    unsafe { platform::volume_identity(drive_letter) }
}

/// Enumerate current Windows volume GUID roots without assuming that a volume has a DOS drive
/// letter. These are access paths for the current boot only and must not be persisted as identity.
#[cfg(windows)]
pub fn volume_guid_paths() -> Result<Vec<String>, StorageError> {
    unsafe { platform::volume_guid_paths() }
}

#[cfg(not(windows))]
pub fn volume_guid_paths() -> Result<Vec<String>, StorageError> {
    Err(StorageError::new(
        "enumerate volume GUID paths",
        "Windows storage APIs are unavailable",
    ))
}

/// Read the current single physical extent behind a volume GUID root.
#[cfg(windows)]
pub fn volume_identity_from_guid_path(
    volume_guid_root: &str,
) -> Result<VolumeIdentity, StorageError> {
    unsafe { platform::volume_identity_from_guid_path(volume_guid_root) }
}

#[cfg(not(windows))]
pub fn volume_identity_from_guid_path(
    _volume_guid_root: &str,
) -> Result<VolumeIdentity, StorageError> {
    Err(StorageError::new(
        "resolve volume GUID extent",
        "Windows storage APIs are unavailable",
    ))
}

/// Read the stable disk, partition and single-extent identity behind an exact current-session
/// volume GUID root without assigning a DOS drive letter.
///
/// Microsoft documents volume GUID roots in the `\\?\Volume{GUID}\` form. The trailing slash is
/// retained for validating that exact root and removed only by the existing volume-open boundary;
/// the physical extent is then read through `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` before the
/// stable disk and partition identity is resolved. Volume GUID paths are access paths for the
/// current boot and must not themselves be persisted as identity.
#[cfg(windows)]
pub fn stable_volume_identity_from_guid_path(
    volume_guid_root: &str,
) -> Result<StableVolumeIdentity, StorageError> {
    unsafe { platform::stable_volume_identity_from_guid_path(volume_guid_root) }
}

#[cfg(not(windows))]
pub fn stable_volume_identity_from_guid_path(
    _volume_guid_root: &str,
) -> Result<StableVolumeIdentity, StorageError> {
    Err(StorageError::new(
        "resolve stable volume GUID identity",
        "Windows storage APIs are unavailable",
    ))
}

#[cfg(windows)]
pub fn stable_volume_identity(drive_letter: char) -> Result<StableVolumeIdentity, StorageError> {
    unsafe { platform::stable_volume_identity(drive_letter) }
}

/// Enumerate every currently present physical disk through SetupAPI and resolve its current number
/// with `IOCTL_STORAGE_GET_DEVICE_NUMBER`.
///
/// SetupAPI device paths are opaque CreateFileW locators. They are never parsed for a
/// `PhysicalDriveN` suffix, because Microsoft does not define such a naming contract. The result is
/// sorted and deduplicated and includes present RAW/unallocated disks.
#[cfg(windows)]
pub fn physical_disk_numbers() -> Result<Vec<u32>, StorageError> {
    unsafe { platform::physical_disk_numbers() }
}

#[cfg(not(windows))]
pub fn physical_disk_numbers() -> Result<Vec<u32>, StorageError> {
    Err(StorageError::new(
        "enumerate physical disks",
        "Windows storage APIs are unavailable",
    ))
}

/// Capture a canonical, provider-independent physical-disk layout snapshot.
///
/// Both the normal endpoint and WinPE must use this shared IOCTL boundary for handoff
/// fingerprints. VDS partition enumeration is intentionally not part of this representation.
#[cfg(windows)]
pub fn disk_layout_snapshot(disk_number: u32) -> Result<DiskLayoutSnapshot, StorageError> {
    unsafe { platform::disk_layout_snapshot(disk_number) }
}

/// Revalidate a canonical snapshot through an already-open PhysicalDrive handle.
///
/// The caller must pass a valid disk handle opened with `GENERIC_READ`; the handle remains owned
/// by the caller. This is intended for raw write paths so the final identity check and the first
/// write are bound to the same kernel file object.
///
/// # Safety
///
/// `handle` must be a valid, open `PhysicalDrive` handle with `GENERIC_READ` access for the disk
/// represented by `expected`, and it must remain valid for the duration of this call.
#[cfg(windows)]
pub unsafe fn verify_disk_layout_snapshot_from_physical_handle(
    handle: windows::Win32::Foundation::HANDLE,
    expected: &DiskLayoutSnapshot,
) -> Result<(), StorageError> {
    platform::verify_disk_layout_snapshot_from_handle(handle, expected)
}

#[cfg(not(windows))]
pub fn disk_layout_snapshot(_disk_number: u32) -> Result<DiskLayoutSnapshot, StorageError> {
    Err(StorageError::new(
        "snapshot physical disk layout",
        "Windows storage APIs are unavailable",
    ))
}

#[cfg(windows)]
pub fn volume_guid_path_for_partition(
    disk_number: u32,
    offset_bytes: u64,
) -> Result<String, StorageError> {
    unsafe { platform::volume_guid_path_for_partition(disk_number, offset_bytes) }
}

/// Try to resolve a partition through the ordinary volume GUID namespace without changing its
/// access paths. Hidden OEM/ESP partitions are allowed to return `Ok(None)` because Microsoft
/// documents that they need not be enumerable as ordinary volume objects.
#[cfg(windows)]
pub fn try_volume_guid_path_for_partition(
    disk_number: u32,
    offset_bytes: u64,
) -> Result<Option<String>, StorageError> {
    unsafe { platform::try_volume_guid_path_for_partition(disk_number, offset_bytes) }
}

#[cfg(windows)]
pub fn mbr_signature(disk_number: u32) -> Result<Option<u32>, StorageError> {
    unsafe { platform::mbr_signature(disk_number) }
}

#[cfg(windows)]
pub fn set_mbr_signature(disk_number: u32, signature: u32) -> Result<(), StorageError> {
    unsafe { platform::set_mbr_signature(disk_number, signature) }
}

#[cfg(windows)]
pub fn disk_style(disk_number: u32) -> Result<DiskStyle, StorageError> {
    unsafe { platform::disk_style(disk_number) }
}

#[cfg(windows)]
pub fn vds_disk_size(disk_number: u32) -> Result<u64, StorageError> {
    unsafe { platform::vds_disk_size(disk_number) }
}

#[cfg(windows)]
pub fn disk_bus_type(disk_number: u32) -> Result<DiskBusType, StorageError> {
    unsafe { platform::disk_bus_type(disk_number) }
}

/// Query authoritative logical/physical sector geometry for the current physical disk.
///
/// This is a read-only `IOCTL_STORAGE_QUERY_PROPERTY(StorageAccessAlignmentProperty)` query,
/// available since Windows Vista. No fallback geometry is invented when the device stack rejects
/// the property or returns an incomplete/contradictory descriptor.
#[cfg(windows)]
pub fn physical_disk_sector_geometry(disk_number: u32) -> Result<DiskSectorGeometry, StorageError> {
    unsafe { platform::physical_disk_sector_geometry(disk_number) }
}

#[cfg(not(windows))]
pub fn physical_disk_sector_geometry(
    _disk_number: u32,
) -> Result<DiskSectorGeometry, StorageError> {
    Err(StorageError::new(
        "query physical disk sector geometry",
        "Windows storage APIs are unavailable",
    ))
}

#[cfg(windows)]
pub fn contiguous_free_bytes_after(
    disk_number: u32,
    end_offset_bytes: u64,
) -> Result<u64, StorageError> {
    unsafe { platform::contiguous_free_bytes_after(disk_number, end_offset_bytes) }
}

#[cfg(windows)]
pub fn current_free_extents(disk_number: u32) -> Result<Vec<FreeExtent>, StorageError> {
    unsafe { platform::current_free_extents(disk_number) }
}

#[cfg(windows)]
pub fn partitions(disk_number: u32) -> Result<Vec<PartitionRecord>, StorageError> {
    unsafe { platform::partitions(disk_number) }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::platform::{classify_add_access_path_result, verify_added_access_path_with};
    use super::*;
    #[cfg(windows)]
    use windows::core::HRESULT;

    #[test]
    fn storage_management_shrink_prefers_exact_desired_final_size() {
        let current = 100_000_513;
        let desired_reclaim = 30_000_257;
        let minimum_reclaim = 20_000_129;
        assert_eq!(
            storage_management_shrink_target(
                current,
                desired_reclaim,
                minimum_reclaim,
                60_000_111,
                current,
            )
            .unwrap(),
            current - desired_reclaim
        );
    }

    #[test]
    fn storage_management_shrink_accepts_provider_minimum_when_minimum_reclaim_is_met() {
        let current = 100_000_513;
        let desired_reclaim = 40_000_257;
        let minimum_reclaim = 20_000_129;
        let provider_minimum = 70_000_333;
        assert_eq!(
            storage_management_shrink_target(
                current,
                desired_reclaim,
                minimum_reclaim,
                provider_minimum,
                current,
            )
            .unwrap(),
            provider_minimum,
            "the provider's byte-exact non-MiB minimum is authoritative"
        );
    }

    #[test]
    fn storage_management_shrink_rejects_provider_minimum_below_required_reclaim() {
        let current = 100_000_513;
        assert!(storage_management_shrink_target(
            current,
            40_000_257,
            20_000_129,
            current - 20_000_128,
            current,
        )
        .is_err());
    }

    #[test]
    fn storage_management_shrink_rejects_invalid_ranges_and_never_rounds_geometry() {
        assert!(storage_management_shrink_target(10_003, 0, 1, 1, 10_003).is_err());
        assert!(storage_management_shrink_target(10_003, 2_003, 2_004, 1, 10_003).is_err());
        assert!(storage_management_shrink_target(10_003, 2_003, 1_003, 9_001, 9_000).is_err());
        assert_eq!(
            storage_management_shrink_target(10_003, 2_003, 1_003, 7_999, 10_003).unwrap(),
            8_000
        );
    }

    #[test]
    fn sector_geometry_accepts_authoritative_512e_and_4kn_descriptors() {
        assert_eq!(
            validated_disk_sector_geometry(28, 28, 28, 28, 512, 512, 0).unwrap(),
            DiskSectorGeometry {
                logical_sector_bytes: 512,
                physical_sector_bytes: 512,
                sector_alignment_offset_bytes: 0,
            },
            "512n is a distinct supported geometry"
        );
        assert_eq!(
            validated_disk_sector_geometry(28, 28, 28, 28, 512, 4096, 0).unwrap(),
            DiskSectorGeometry {
                logical_sector_bytes: 512,
                physical_sector_bytes: 4096,
                sector_alignment_offset_bytes: 0,
            }
        );
        assert_eq!(
            validated_disk_sector_geometry(32, 32, 28, 28, 4096, 4096, 0).unwrap(),
            DiskSectorGeometry {
                logical_sector_bytes: 4096,
                physical_sector_bytes: 4096,
                sector_alignment_offset_bytes: 0,
            },
            "a future larger descriptor remains compatible when every known field was returned"
        );
        assert_eq!(
            validated_disk_sector_geometry(28, 28, 28, 28, 520, 4160, 520).unwrap(),
            DiskSectorGeometry {
                logical_sector_bytes: 520,
                physical_sector_bytes: 4160,
                sector_alignment_offset_bytes: 520,
            },
            "authoritative non-power-of-two sector sizes must not be replaced by 512/4096 guesses"
        );
    }

    #[test]
    fn sector_geometry_rejects_truncation_zero_and_contradictory_fields_without_defaults() {
        assert!(validated_disk_sector_geometry(28, 28, 27, 28, 512, 4096, 0).is_err());
        assert!(validated_disk_sector_geometry(27, 28, 28, 28, 512, 4096, 0).is_err());
        assert!(validated_disk_sector_geometry(28, 27, 28, 28, 512, 4096, 0).is_err());
        assert!(validated_disk_sector_geometry(28, 28, 28, 28, 0, 4096, 0).is_err());
        assert!(validated_disk_sector_geometry(28, 28, 28, 28, 512, 0, 0).is_err());
        assert!(validated_disk_sector_geometry(28, 28, 28, 28, 4096, 512, 0).is_err());
        assert!(validated_disk_sector_geometry(28, 28, 28, 28, 768, 4096, 0).is_err());
        assert!(validated_disk_sector_geometry(28, 28, 28, 28, 512, 4096, 4096).is_err());
        assert!(validated_disk_sector_geometry(28, 28, 28, 28, 512, 4096, 1).is_err());
    }

    fn request(kind: PartitionKind) -> CreatePartitionRequest {
        CreatePartitionRequest {
            disk_number: 2,
            offset_bytes: 0,
            size_bytes: 1024 * 1024,
            kind,
            file_system: Some(FileSystem::Ntfs),
            label: "Data".into(),
            drive_letter: Some('D'),
            active: false,
            preserve_gpt_metadata: None,
        }
    }

    #[test]
    fn validates_partition_role_constraints_before_windows_io() {
        let mut value = request(PartitionKind::EfiSystem);
        assert!(validate_create_request(&value).is_err());
        value.file_system = Some(FileSystem::Fat32);
        value.drive_letter = None;
        assert!(validate_create_request(&value).is_ok());

        let mut value = request(PartitionKind::MicrosoftReserved);
        assert!(validate_create_request(&value).is_err());
        value.file_system = None;
        value.drive_letter = None;
        assert!(validate_create_request(&value).is_ok());
    }

    #[test]
    fn partition_creation_requires_a_nonzero_minimum_capacity() {
        let mut value = request(PartitionKind::BasicData);
        value.size_bytes = 0;
        assert!(validate_create_request(&value).is_err());

        value.size_bytes = 2;
        value.offset_bytes = u64::MAX;
        assert!(validate_create_request(&value).is_err());
    }

    #[test]
    fn accepts_provider_geometry_but_rejects_invalid_letters_and_control_characters() {
        let mut value = request(PartitionKind::BasicData);
        value.offset_bytes = 4096 + 512;
        value.size_bytes = 64 * 1024 + 512;
        assert!(validate_create_request(&value).is_ok());
        value.drive_letter = Some('A');
        assert!(validate_create_request(&value).is_err());
        value.drive_letter = Some('D');
        value.label = "bad\nlabel".into();
        assert!(validate_create_request(&value).is_err());
    }

    #[test]
    fn post_create_format_failure_invokes_exact_rollback_and_preserves_diagnostics() {
        let mut rollback_called = false;
        let result = require_post_create_or_rollback::<()>(
            Err(StorageError::new(
                "format created volume",
                "mock format failure",
            )),
            || {
                rollback_called = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(rollback_called);
        assert!(result.to_string().contains("mock format failure"));
        assert!(result.to_string().contains("was rolled back"));

        let result = require_post_create_or_rollback::<()>(
            Err(StorageError::new(
                "assign access path",
                "mock mount failure",
            )),
            || {
                Err(StorageError::new(
                    "delete partition",
                    "mock rollback failure",
                ))
            },
        )
        .unwrap_err();
        assert!(result.to_string().contains("mock mount failure"));
        assert!(result.to_string().contains("mock rollback failure"));
    }

    #[test]
    #[cfg(windows)]
    fn add_access_path_accepts_only_documented_success_results() {
        assert!(!classify_add_access_path_result(HRESULT(0)).unwrap());
        assert!(classify_add_access_path_result(HRESULT(1)).unwrap());
        assert!(classify_add_access_path_result(HRESULT(0x8004_2431u32 as i32)).is_err());
    }

    #[test]
    #[cfg(windows)]
    fn add_access_path_readback_allows_mount_manager_delay_then_requires_exact_extent() {
        use std::collections::VecDeque;

        let expected = VolumeIdentity {
            disk_number: 2,
            offset_bytes: 4096 + 512,
            extent_length_bytes: 64 * 1024 * 1024 + 512,
        };
        let mut values = VecDeque::from([
            Err(StorageError::new("open assigned volume", "not ready")),
            Err(StorageError::new("open assigned volume", "still not ready")),
            Ok(expected),
        ]);
        let mut waits = 0;
        verify_added_access_path_with(
            expected,
            || values.pop_front().expect("bounded readback call"),
            || waits += 1,
        )
        .unwrap();
        assert_eq!(waits, 2);

        let wrong = VolumeIdentity {
            disk_number: 9,
            ..expected
        };
        let error = verify_added_access_path_with(expected, || Ok(wrong), || {})
            .expect_err("a concurrently rebound letter must be rejected");
        assert!(error.to_string().contains("resolves to disk 9"));
    }

    #[test]
    #[cfg(windows)]
    fn add_access_path_readback_times_out_without_claiming_success() {
        let expected = VolumeIdentity {
            disk_number: 2,
            offset_bytes: 4096 + 512,
            extent_length_bytes: 64 * 1024 * 1024 + 512,
        };
        let mut reads = 0;
        let mut waits = 0;
        let error = verify_added_access_path_with(
            expected,
            || {
                reads += 1;
                Err(StorageError::new("open assigned volume", "not ready"))
            },
            || waits += 1,
        )
        .expect_err("an unreadable access path must not be accepted");
        assert_eq!(reads, 50);
        assert_eq!(waits, 49);
        assert!(error.to_string().contains("within 5 seconds"));
    }

    #[test]
    fn extend_readback_accepts_provider_rounding_within_the_authorized_free_extent() {
        let expected = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 4096,
            extent_length_bytes: 10 * 1024 * 1024 + 512,
        };
        let provider_start = expected.offset_bytes + expected.extent_length_bytes;
        let actual = VolumeIdentity {
            extent_length_bytes: expected.extent_length_bytes + 2 * 1024 * 1024 + 4096,
            ..expected
        };
        assert_eq!(
            verified_extend_added_bytes(
                expected,
                actual,
                2 * 1024 * 1024,
                provider_start,
                provider_start + 8 * 1024 * 1024,
            ),
            Ok(2 * 1024 * 1024 + 4096)
        );
        assert!(verified_extend_added_bytes(
            expected,
            actual,
            2 * 1024 * 1024,
            provider_start + 512,
            provider_start + 8 * 1024 * 1024,
        )
        .is_err());
    }

    #[test]
    fn temporary_mount_cleanup_requires_the_complete_volume_extent() {
        let expected = VolumeIdentity {
            disk_number: 2,
            offset_bytes: 1_048_576,
            extent_length_bytes: 268_435_456,
        };
        assert!(same_volume_identity(expected, expected));
        assert!(!same_volume_identity(
            expected,
            VolumeIdentity {
                disk_number: 3,
                ..expected
            }
        ));
        assert!(!same_volume_identity(
            expected,
            VolumeIdentity {
                offset_bytes: 2_097_152,
                ..expected
            }
        ));
        assert!(!same_volume_identity(
            expected,
            VolumeIdentity {
                extent_length_bytes: expected.extent_length_bytes / 2,
                ..expected
            }
        ));
    }

    #[test]
    fn format_source_drive_parser_accepts_only_drive_rooted_windows_paths() {
        assert_eq!(
            path_drive_letter(std::path::Path::new(r"F:\images\install.wim")),
            Some('F')
        );
        assert_eq!(
            path_drive_letter(std::path::Path::new(r"\\?\e:\staged\install.esd")),
            Some('E')
        );
        assert_eq!(
            path_drive_letter(std::path::Path::new(r"\\.\D:\source.gho")),
            Some('D')
        );
        assert_eq!(
            path_drive_letter(std::path::Path::new(r"\\server\share\install.wim")),
            None
        );
        assert_eq!(path_drive_letter(std::path::Path::new("install.wim")), None);
    }

    #[test]
    fn checked_format_identity_requires_the_complete_extent() {
        let expected = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 1_048_576,
            extent_length_bytes: 64 * 1024 * 1024,
        };
        assert!(same_volume_identity(expected, expected));
        assert!(!same_volume_identity(
            expected,
            VolumeIdentity {
                extent_length_bytes: expected.extent_length_bytes + 4096,
                ..expected
            }
        ));
    }

    fn test_disk_snapshot(device_id_hash: Option<[u8; 32]>) -> DiskLayoutSnapshot {
        DiskLayoutSnapshot {
            disk_size_bytes: 80 * 1024 * 1024 * 1024,
            disk: StableDiskIdentity::Gpt { disk_id: [4; 16] },
            device_id_hash,
            partitions: vec![DiskLayoutPartitionSnapshot {
                // Deliberately sector aligned but not MiB aligned.
                offset_bytes: 1_048_576 + 512,
                size_bytes: 32 * 1024 * 1024 * 1024 + 4096,
                token: DiskLayoutPartitionToken::Gpt {
                    partition_type: GPT_BASIC_DATA_PARTITION_TYPE,
                    partition_id: [8; 16],
                    attributes: 0,
                },
            }],
        }
    }

    #[test]
    fn present_disk_aliases_must_agree_before_any_destructive_use() {
        let trusted = test_disk_snapshot(Some([7; 32]));
        let (_, merged) = reconcile_present_disk_snapshots(
            3,
            vec![
                ("filter-alias".to_owned(), test_disk_snapshot(None)),
                ("physical-interface".to_owned(), trusted.clone()),
            ],
        )
        .expect("an alias without a device ID may agree with an identified path");
        assert_eq!(merged, trusted);

        let error = reconcile_present_disk_snapshots(3, Vec::new())
            .expect_err("a current number without a present interface is not a disk identity");
        assert!(error.to_string().contains("UntrustedStorage"));
    }

    #[test]
    fn conflicting_filter_observations_enter_untrusted_storage() {
        let baseline = test_disk_snapshot(Some([7; 32]));
        let mut conflicts = Vec::new();

        let mut capacity = baseline.clone();
        capacity.disk_size_bytes += 4096;
        conflicts.push(capacity);

        let mut partition = baseline.clone();
        partition.partitions[0].size_bytes += 512;
        conflicts.push(partition);

        let mut disk_id = baseline.clone();
        disk_id.disk = StableDiskIdentity::Gpt { disk_id: [5; 16] };
        conflicts.push(disk_id);

        let mut device_id = baseline.clone();
        device_id.device_id_hash = Some([9; 32]);
        conflicts.push(device_id);

        for conflict in conflicts {
            let error = reconcile_present_disk_snapshots(
                3,
                vec![
                    ("trusted-path".to_owned(), baseline.clone()),
                    ("spoofed-path".to_owned(), conflict),
                ],
            )
            .expect_err("contradictory present interfaces must stop before write");
            assert!(error.to_string().contains("UntrustedStorage"));
        }
    }

    #[test]
    fn conflicting_bus_descriptors_enter_untrusted_storage() {
        assert_eq!(
            reconcile_present_disk_bus_types(
                3,
                vec![
                    ("filter-alias".to_owned(), DiskBusType::Nvme),
                    ("physical-interface".to_owned(), DiskBusType::Nvme),
                ],
            )
            .unwrap(),
            DiskBusType::Nvme
        );

        let error = reconcile_present_disk_bus_types(
            3,
            vec![
                ("filter-alias".to_owned(), DiskBusType::Other),
                ("physical-interface".to_owned(), DiskBusType::Nvme),
            ],
        )
        .expect_err("contradictory successful bus descriptors must not be guessed");
        assert!(error.to_string().contains("UntrustedStorage"));

        let error = reconcile_present_disk_bus_types(3, Vec::new())
            .expect_err("a disk number alone is not a bus-type observation");
        assert!(error.to_string().contains("UntrustedStorage"));
    }

    #[test]
    fn volume_guid_extent_and_partition_layout_form_one_write_boundary() {
        let snapshot = test_disk_snapshot(Some([7; 32]));
        let extent = VolumeIdentity {
            disk_number: 3,
            offset_bytes: snapshot.partitions[0].offset_bytes,
            extent_length_bytes: snapshot.partitions[0].size_bytes,
        };
        assert_eq!(
            verify_current_volume_identity_closure(extent, extent, &snapshot).unwrap(),
            snapshot.partitions[0]
        );

        for conflicting_guid in [
            VolumeIdentity {
                disk_number: 4,
                ..extent
            },
            VolumeIdentity {
                offset_bytes: extent.offset_bytes + 512,
                ..extent
            },
            VolumeIdentity {
                extent_length_bytes: extent.extent_length_bytes - 512,
                ..extent
            },
        ] {
            let error = verify_current_volume_identity_closure(extent, conflicting_guid, &snapshot)
                .expect_err("a contradictory volume-GUID path must stop before write");
            assert!(error.to_string().contains("UntrustedStorage"));
        }

        let mut missing = snapshot.clone();
        missing.partitions.clear();
        let error = verify_current_volume_identity_closure(extent, extent, &missing)
            .expect_err("an extent without an exact partition record is untrusted");
        assert!(error.to_string().contains("UntrustedStorage"));
    }

    #[test]
    fn invalid_function_device_number_query_uses_real_extended_api_result() {
        let legacy = Err(StorageError::new(
            "IOCTL_STORAGE_GET_DEVICE_NUMBER",
            "The request is not supported (Win32 1 / ERROR_INVALID_FUNCTION)",
        ));
        let resolved = resolve_device_number_with_extended_fallback(legacy, || Ok(17_u32))
            .expect("the documented extended query may supply the current locator");
        assert_eq!(resolved, 17);
    }

    #[test]
    fn both_device_number_queries_failing_never_defaults_to_disk_zero() {
        let error = resolve_device_number_with_extended_fallback::<u32>(
            Err(StorageError::new(
                "IOCTL_STORAGE_GET_DEVICE_NUMBER",
                "ERROR_INVALID_FUNCTION",
            )),
            || {
                Err(StorageError::new(
                    "IOCTL_STORAGE_GET_DEVICE_NUMBER_EX",
                    "ERROR_NOT_SUPPORTED",
                ))
            },
        )
        .expect_err("without an authoritative current number the interface must be rejected");
        let message = error.to_string();
        assert!(message.contains("ERROR_INVALID_FUNCTION"));
        assert!(message.contains("ERROR_NOT_SUPPORTED"));
        assert!(!message.contains("default"));
    }

    #[cfg(windows)]
    #[test]
    fn optional_storage_device_id_property_accepts_only_documented_unavailable_statuses() {
        use windows::core::HRESULT;
        use windows::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
            ERROR_NOT_SUPPORTED,
        };

        for error in [
            ERROR_INVALID_FUNCTION,
            ERROR_INVALID_PARAMETER,
            ERROR_NOT_SUPPORTED,
        ] {
            assert!(platform::storage_device_id_property_unavailable(
                HRESULT::from_win32(error.0)
            ));
        }
        assert!(!platform::storage_device_id_property_unavailable(
            HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        ));
    }

    #[test]
    fn storage_management_partition_number_is_only_a_current_extent_locator() {
        let expected = VolumeIdentity {
            disk_number: 7,
            offset_bytes: 1_048_576 + 512,
            extent_length_bytes: 40_000_000_512,
        };
        assert!(storage_management_partition_matches_current_extent(
            'E',
            7,
            4,
            expected.extent_length_bytes,
            'E',
            expected,
            4,
        ));
        assert!(!storage_management_partition_matches_current_extent(
            'F',
            7,
            4,
            expected.extent_length_bytes,
            'E',
            expected,
            4,
        ));
        assert!(!storage_management_partition_matches_current_extent(
            'E',
            8,
            4,
            expected.extent_length_bytes,
            'E',
            expected,
            4,
        ));
        assert!(!storage_management_partition_matches_current_extent(
            'E',
            7,
            5,
            expected.extent_length_bytes,
            'E',
            expected,
            4,
        ));
        assert!(!storage_management_partition_matches_current_extent(
            'E',
            7,
            4,
            expected.extent_length_bytes + 512,
            'E',
            expected,
            4,
        ));
    }

    fn stable_mbr(partition_number: u32, device_id_hash: Option<[u8; 32]>) -> StableVolumeIdentity {
        StableVolumeIdentity {
            extent: VolumeIdentity {
                disk_number: 3,
                offset_bytes: 1_048_576,
                extent_length_bytes: 64 * 1024 * 1024,
            },
            disk: StableDiskIdentity::Mbr {
                signature: 0x1234_5678,
            },
            partition: StablePartitionIdentity::Mbr { partition_number },
            device_id_hash,
        }
    }

    #[test]
    fn mbr_partition_number_is_not_an_identity_token_but_device_id_is() {
        let expected = stable_mbr(2, Some([7; 32]));
        assert!(same_stable_volume_identity(
            expected,
            stable_mbr(9, Some([7; 32]))
        ));
        assert!(!same_stable_volume_identity(
            expected,
            stable_mbr(2, Some([8; 32]))
        ));
        assert!(!same_stable_volume_identity(expected, stable_mbr(2, None)));
        assert!(!same_stable_volume_identity(
            expected,
            StableVolumeIdentity {
                disk: StableDiskIdentity::Mbr {
                    signature: 0x8765_4321,
                },
                ..expected
            }
        ));
        assert!(!same_stable_volume_identity(
            expected,
            StableVolumeIdentity {
                extent: VolumeIdentity {
                    offset_bytes: expected.extent.offset_bytes + 4096,
                    ..expected.extent
                },
                ..expected
            }
        ));
    }

    #[test]
    fn install_target_token_accepts_only_ordinary_visible_user_data() {
        let basic = GPT_BASIC_DATA_PARTITION_TYPE;
        let partition_id = [7; 16];
        assert!(partition_token_is_installable_user_data(
            DiskLayoutPartitionToken::Gpt {
                partition_type: basic,
                partition_id,
                attributes: 0,
            }
        ));
        for attribute in [
            GPT_ATTRIBUTE_PLATFORM_REQUIRED,
            GPT_BASIC_DATA_ATTRIBUTE_READ_ONLY,
            GPT_BASIC_DATA_ATTRIBUTE_SHADOW_COPY,
            GPT_BASIC_DATA_ATTRIBUTE_HIDDEN,
            GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER,
        ] {
            assert!(!partition_token_is_installable_user_data(
                DiskLayoutPartitionToken::Gpt {
                    partition_type: basic,
                    partition_id,
                    attributes: attribute,
                }
            ));
        }
        assert!(!partition_token_is_installable_user_data(
            DiskLayoutPartitionToken::Gpt {
                partition_type: 0xde94_bba4_06d1_4d40_a16a_bfd5_0179_d6ac_u128.to_le_bytes(),
                partition_id,
                attributes: 0,
            }
        ));
        assert!(partition_token_is_installable_user_data(
            DiskLayoutPartitionToken::Mbr {
                partition_type: 0x07,
                boot_indicator: false,
            }
        ));
        assert!(!partition_token_is_installable_user_data(
            DiskLayoutPartitionToken::Mbr {
                partition_type: 0x27,
                boot_indicator: false,
            }
        ));
    }

    #[test]
    fn canonical_layout_digest_ignores_provider_enumeration_order() {
        let first = DiskLayoutPartitionSnapshot {
            offset_bytes: 1_048_576,
            size_bytes: 100 * 1024 * 1024,
            token: DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: true,
            },
        };
        let second = DiskLayoutPartitionSnapshot {
            offset_bytes: 201 * 1024 * 1024,
            size_bytes: 200 * 1024 * 1024,
            token: DiskLayoutPartitionToken::Mbr {
                partition_type: 7,
                boot_indicator: false,
            },
        };
        let mut snapshot = DiskLayoutSnapshot {
            disk_size_bytes: 500 * 1024 * 1024,
            disk: StableDiskIdentity::Mbr {
                signature: 0x1234_5678,
            },
            device_id_hash: Some([9; 32]),
            partitions: vec![second, first],
        };
        let digest = disk_layout_snapshot_digest(&snapshot);
        snapshot.partitions.reverse();
        assert_eq!(digest, disk_layout_snapshot_digest(&snapshot));
        snapshot.partitions[0].size_bytes += 4096;
        assert_ne!(digest, disk_layout_snapshot_digest(&snapshot));
    }
}
