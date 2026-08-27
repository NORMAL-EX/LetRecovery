//! Strict syntax gate for the normal-endpoint to WinPE installation handoff.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};

use crate::windows_storage::{
    disk_layout_snapshot_digest, DiskLayoutPartitionToken, DiskLayoutSnapshot, StableDiskIdentity,
};

pub const CANONICAL_TARGET_VERSION: u8 = 2;
/// Fixed leaf used only to rediscover the user-selected installation volume after booting PE.
/// The value is an independent 256-bit CNG locator token authenticated by the private LRHM3
/// manifest; it is not a second trust root and carries no disk inventory fields.
pub const DATA_VOLUME_MARKER_NAME: &str = "LetRecovery_Data.marker";
pub const INSTALL_TARGET_MARKER_NAME: &str = "LetRecovery_Target.marker";
/// Per-selected-disk random locator used by authenticated full-disk reinstall plans.
pub const FULL_DISK_MARKER_NAME: &str = "LetRecovery_FullDisk.marker";

pub fn locator_marker_bytes(token: &str) -> Result<Vec<u8>> {
    crate::handoff_auth::validate_locator_token(token)?;
    Ok(token.as_bytes().to_vec())
}

/// Mismatched or malformed same-name files are unrelated files, not installation errors.
pub fn locator_marker_matches(bytes: &[u8], token: &str) -> bool {
    locator_marker_bytes(token).is_ok_and(|expected| expected.as_slice() == bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalTargetStyle {
    Gpt,
    Mbr,
}

impl CanonicalTargetStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpt => "GPT",
            Self::Mbr => "MBR",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "GPT" => Ok(Self::Gpt),
            "MBR" => Ok(Self::Mbr),
            _ => bail!("invalid canonical target disk style: {value}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalInstallTargetV2 {
    pub layout_digest: [u8; 32],
    /// Optional StorageIdAssocDevice evidence captured by the normal endpoint. It is deliberately
    /// separate from the portable layout digest because some WinPE storage stacks cannot expose
    /// the property even when normal Windows can. When both sides expose it, it must match.
    pub device_id_hash: Option<[u8; 32]>,
    pub partition_offset_bytes: u64,
    pub partition_length_bytes: u64,
    pub style: CanonicalTargetStyle,
    pub gpt_partition_id: Option<[u8; 16]>,
}

impl CanonicalInstallTargetV2 {
    fn portable_layout_digest(snapshot: &DiskLayoutSnapshot) -> [u8; 32] {
        let mut portable = snapshot.clone();
        portable.device_id_hash = None;
        disk_layout_snapshot_digest(&portable)
    }

    pub fn from_snapshot(
        snapshot: &DiskLayoutSnapshot,
        partition_offset_bytes: u64,
        partition_length_bytes: u64,
    ) -> Result<Self> {
        if partition_length_bytes == 0 {
            bail!("canonical target partition length must be nonzero");
        }
        let partition = snapshot
            .partitions
            .iter()
            .find(|partition| {
                partition.offset_bytes == partition_offset_bytes
                    && partition.size_bytes == partition_length_bytes
            })
            .context("canonical target extent is absent from disk layout")?;
        let (style, gpt_partition_id) = match (snapshot.disk, partition.token) {
            (
                StableDiskIdentity::Gpt { .. },
                DiskLayoutPartitionToken::Gpt { partition_id, .. },
            ) if partition_id != [0; 16] => (CanonicalTargetStyle::Gpt, Some(partition_id)),
            (StableDiskIdentity::Mbr { signature }, DiskLayoutPartitionToken::Mbr { .. })
                if signature != 0 =>
            {
                (CanonicalTargetStyle::Mbr, None)
            }
            _ => bail!("canonical target disk and partition styles are inconsistent or unstable"),
        };
        Ok(Self {
            layout_digest: Self::portable_layout_digest(snapshot),
            device_id_hash: snapshot.device_id_hash,
            partition_offset_bytes,
            partition_length_bytes,
            style,
            gpt_partition_id,
        })
    }

    pub fn matches_snapshot(&self, snapshot: &DiskLayoutSnapshot) -> bool {
        if self.partition_length_bytes == 0
            || Self::portable_layout_digest(snapshot) != self.layout_digest
        {
            return false;
        }
        if let (Some(expected), Some(actual)) = (self.device_id_hash, snapshot.device_id_hash) {
            if expected != actual {
                return false;
            }
        }
        snapshot.partitions.iter().any(|partition| {
            if partition.offset_bytes != self.partition_offset_bytes
                || partition.size_bytes != self.partition_length_bytes
            {
                return false;
            }
            match (
                self.style,
                self.gpt_partition_id,
                snapshot.disk,
                partition.token,
            ) {
                (
                    CanonicalTargetStyle::Gpt,
                    Some(expected_id),
                    StableDiskIdentity::Gpt { .. },
                    DiskLayoutPartitionToken::Gpt { partition_id, .. },
                ) => expected_id == partition_id,
                (
                    CanonicalTargetStyle::Mbr,
                    None,
                    StableDiskIdentity::Mbr { signature },
                    DiskLayoutPartitionToken::Mbr { .. },
                ) => signature != 0,
                _ => false,
            }
        })
    }
}

pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub fn decode_hex_array<const N: usize>(value: &str, field: &str) -> Result<[u8; N]> {
    let value = value.trim();
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "{field} must contain exactly {} hexadecimal characters",
            N * 2
        );
    }
    let mut output = [0_u8; N];
    for (index, slot) in output.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .with_context(|| format!("invalid hexadecimal {field}"))?;
    }
    Ok(output)
}

pub fn unique_canonical_target_match(
    target: &CanonicalInstallTargetV2,
    candidates: &[(u32, DiskLayoutSnapshot)],
) -> Result<u32> {
    let matches = candidates
        .iter()
        .filter(|(_, snapshot)| target.matches_snapshot(snapshot))
        .map(|(disk_number, _)| *disk_number)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [disk_number] => Ok(*disk_number),
        [] => bail!("canonical installation target no longer matches any physical disk"),
        _ => bail!("canonical installation target matches multiple cloned physical disks"),
    }
}

pub fn canonical_target_from_fields(
    version: Option<u8>,
    layout_digest: Option<&str>,
    partition_offset_bytes: Option<u64>,
    partition_length_bytes: Option<u64>,
    style: Option<&str>,
    gpt_partition_id: Option<&str>,
    device_id_hash: Option<&str>,
) -> Result<Option<CanonicalInstallTargetV2>> {
    let present = [
        version.is_some(),
        layout_digest.is_some(),
        partition_offset_bytes.is_some(),
        partition_length_bytes.is_some(),
        style.is_some(),
        gpt_partition_id.is_some(),
    ];
    if present.iter().all(|value| !value) {
        if device_id_hash.is_some() {
            bail!("canonical storage identity is present without a canonical target");
        }
        return Ok(None);
    }
    if present.iter().any(|value| !value) {
        bail!("canonical V2 installation target fields are incomplete");
    }
    if version != Some(CANONICAL_TARGET_VERSION) {
        bail!("unsupported canonical installation target version");
    }
    let style = CanonicalTargetStyle::parse(style.expect("presence checked"))?;
    let gpt_value = gpt_partition_id.expect("presence checked");
    let gpt_partition_id = match style {
        CanonicalTargetStyle::Gpt => Some(decode_hex_array::<16>(
            gpt_value,
            "CanonicalGptPartitionId",
        )?),
        CanonicalTargetStyle::Mbr if gpt_value == "none" => None,
        CanonicalTargetStyle::Mbr => bail!("MBR canonical target must use GPT partition ID 'none'"),
    };
    let partition_length_bytes = partition_length_bytes.expect("presence checked");
    if partition_length_bytes == 0 {
        bail!("canonical target partition length must be nonzero");
    }
    Ok(Some(CanonicalInstallTargetV2 {
        layout_digest: decode_hex_array::<32>(
            layout_digest.expect("presence checked"),
            "CanonicalDiskLayoutSha256",
        )?,
        device_id_hash: device_id_hash
            .map(|value| decode_hex_array::<32>(value, "CanonicalStorageIdSha256"))
            .transpose()?,
        partition_offset_bytes: partition_offset_bytes.expect("presence checked"),
        partition_length_bytes,
        style,
        gpt_partition_id,
    }))
}

#[derive(Clone, Copy)]
enum ValueKind {
    Text,
    Bool,
    U8 { max: u8 },
    U16,
    U32,
    NonZeroU32,
    BootPca,
    U64,
    Hex { bytes: usize },
    CanonicalStyle,
    CanonicalGptId,
}

fn field_rule(key: &str) -> Option<(&'static str, ValueKind)> {
    let rule = match key {
        "SessionId"
        | "OriginalGUID"
        | "TargetPartition"
        | "ImagePath"
        | "XpSourceArch"
        | "PcaCompatPackage"
        | "PcaCompatSha256"
        | "Language"
        | "CustomInstallPlanJson" => ("Install", ValueKind::Text),
        "Unattended"
        | "RestoreDrivers"
        | "AutoReboot"
        | "AutomationShutdownOnTerminal"
        | "FormatPartition"
        | "PreservePersonalFiles"
        | "RepairBoot"
        | "IsGho"
        | "IsXp"
        | "IsXpI386"
        | "RunDiskpartScripts"
        | "MigrateWifi" => ("Install", ValueKind::Bool),
        "DriverActionMode" => ("Install", ValueKind::U8 { max: 2 }),
        "WimEngine" => ("Install", ValueKind::U8 { max: 1 }),
        "BootMode" => ("Install", ValueKind::U8 { max: 2 }),
        "VolumeIndex" => ("Install", ValueKind::NonZeroU32),
        "BootPcaMode" => ("Install", ValueKind::BootPca),
        "PcaCompatImageIndex" | "PcaCompatTargetBuild" => ("Install", ValueKind::U32),
        "PcaCompatTargetArchitecture" => ("Install", ValueKind::U16),
        "CanonicalTargetVersion" => ("Install", ValueKind::U8 { max: 2 }),
        "CanonicalDiskLayoutSha256" | "CanonicalStorageIdSha256" => {
            ("Install", ValueKind::Hex { bytes: 32 })
        }
        "CanonicalPartitionOffsetBytes" | "CanonicalPartitionLengthBytes" | "WifiProfileLength" => {
            ("Install", ValueKind::U64)
        }
        "WifiProfileSha256" => ("Install", ValueKind::Hex { bytes: 32 }),
        "CanonicalDiskStyle" => ("Install", ValueKind::CanonicalStyle),
        "CanonicalGptPartitionId" => ("Install", ValueKind::CanonicalGptId),
        "HandoffManifestVersion" => ("HandoffManifest", ValueKind::U8 { max: 1 }),
        "HandoffManifestLength" => ("HandoffManifest", ValueKind::U64),
        "HandoffManifestSha256" => ("HandoffManifest", ValueKind::Hex { bytes: 32 }),
        "InstallCabPackages"
        | "RemoveShortcutArrow"
        | "RestoreClassicContextMenu"
        | "BypassNRO"
        | "DisableWindowsUpdate"
        | "DisableWindowsDefender"
        | "DisableReservedStorage"
        | "DisableUAC"
        | "DisableDeviceEncryption"
        | "RemoveUWPApps"
        | "ImportStorageControllerDrivers"
        | "BuiltinAdministrator"
        | "BuiltinAdministratorAutoLogon" => ("Advanced", ValueKind::Bool),
        "CustomUsername"
        | "BuiltinAdministratorName"
        | "BuiltinAdministratorPassword"
        | "VolumeLabel"
        | "CustomUnattendFile"
        | "PreinstalledSoftwareConfig" => ("Advanced", ValueKind::Text),
        "Win7UefiPatch"
        | "Win7InjectUsb3Driver"
        | "Win7InjectNvmeDriver"
        | "Win7FixAcpiBsod"
        | "Win7FixStorageBsod" => ("Win7", ValueKind::Bool),
        "XpInjectUsb3Driver" | "XpInjectNvmeDriver" => ("Xp", ValueKind::Bool),
        _ => return None,
    };
    Some(rule)
}

fn validate_value(key: &str, value: &str, kind: ValueKind) -> Result<()> {
    match kind {
        ValueKind::Text => Ok(()),
        ValueKind::Bool => value
            .parse::<bool>()
            .map(|_| ())
            .with_context(|| format!("invalid boolean for {key}: {value}")),
        ValueKind::U8 { max } => {
            let parsed = value
                .parse::<u8>()
                .with_context(|| format!("invalid integer for {key}: {value}"))?;
            if parsed > max {
                bail!("{key} is outside the supported range 0..={max}: {parsed}");
            }
            Ok(())
        }
        ValueKind::U16 => value
            .parse::<u16>()
            .map(|_| ())
            .with_context(|| format!("invalid integer for {key}: {value}")),
        ValueKind::U32 => value
            .parse::<u32>()
            .map(|_| ())
            .with_context(|| format!("invalid integer for {key}: {value}")),
        ValueKind::NonZeroU32 => {
            let parsed = value
                .parse::<u32>()
                .with_context(|| format!("invalid integer for {key}: {value}"))?;
            if parsed == 0 {
                bail!("{key} must be greater than zero");
            }
            Ok(())
        }
        ValueKind::BootPca => match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "pca2011" | "2011" | "1" | "pca2023" | "2023" | "2" => Ok(()),
            _ => bail!("invalid BootPcaMode: {value}"),
        },
        ValueKind::U64 => value
            .parse::<u64>()
            .map(|_| ())
            .with_context(|| format!("invalid integer for {key}: {value}")),
        ValueKind::Hex { bytes } => {
            if value.len() != bytes * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid hexadecimal value for {key}");
            }
            Ok(())
        }
        ValueKind::CanonicalStyle => CanonicalTargetStyle::parse(value).map(|_| ()),
        ValueKind::CanonicalGptId => {
            if value == "none"
                || (value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                Ok(())
            } else {
                bail!("invalid canonical GPT partition ID")
            }
        }
    }
}

/// Validate every known field that is present without changing legacy defaults
/// for fields that are completely absent.
pub fn validate_install_handoff_ini(content: &str) -> Result<()> {
    let mut section = String::new();
    let mut seen = HashSet::new();

    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') || line.len() < 3 {
                bail!("malformed INI section on line {}", index + 1);
            }
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("malformed INI field on line {}", index + 1))?;
        let key = key.trim();
        let value = value.trim();
        let Some((expected_section, kind)) = field_rule(key) else {
            bail!(
                "unknown install field {key} in section [{section}] on line {}",
                index + 1
            );
        };
        if section != expected_section && section != "Install" {
            bail!(
                "known install field {key} is in unexpected section [{section}] on line {}",
                index + 1
            );
        }
        if !seen.insert(key.to_string()) {
            bail!("duplicate install field {key} on line {}", index + 1);
        }
        validate_value(key, value, kind)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        locator_marker_bytes, locator_marker_matches, unique_canonical_target_match,
        validate_install_handoff_ini, CanonicalInstallTargetV2,
    };

    #[test]
    fn installation_target_marker_ignores_same_name_with_other_contents() {
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let expected = locator_marker_bytes(token).unwrap();
        assert!(locator_marker_matches(&expected, token));
        assert!(!locator_marker_matches(b"unrelated", token));
        assert!(!locator_marker_matches(
            b"fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
            token
        ));
        assert!(!locator_marker_matches(
            b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            token
        ));
    }
    use crate::windows_storage::{
        DiskLayoutPartitionSnapshot, DiskLayoutPartitionToken, DiskLayoutSnapshot,
        StableDiskIdentity,
    };

    fn gpt_snapshot(disk_id: u8, partition_id: u8) -> DiskLayoutSnapshot {
        DiskLayoutSnapshot {
            disk_size_bytes: 1_000_000,
            disk: StableDiskIdentity::Gpt {
                disk_id: [disk_id; 16],
            },
            device_id_hash: Some([disk_id; 32]),
            partitions: vec![DiskLayoutPartitionSnapshot {
                offset_bytes: 1_048_576,
                size_bytes: 500_000,
                token: DiskLayoutPartitionToken::Gpt {
                    partition_type: [3; 16],
                    partition_id: [partition_id; 16],
                    attributes: 0,
                },
            }],
        }
    }

    #[test]
    fn missing_legacy_switches_remain_valid_but_present_corruption_is_rejected() {
        validate_install_handoff_ini("[Install]\nVolumeIndex=1\n").unwrap();
        assert!(
            validate_install_handoff_ini("[Install]\nVolumeIndex=1\nFormatPartition=fasle\n")
                .is_err()
        );
        assert!(validate_install_handoff_ini("[Install]\nVolumeIndex=broken\n").is_err());
        assert!(validate_install_handoff_ini("[Install]\nBootMode=9\n").is_err());
    }

    #[test]
    fn duplicate_and_wrong_section_known_fields_are_rejected() {
        assert!(validate_install_handoff_ini("[Install]\nVolumeIndex=1\nVolumeIndex=2\n").is_err());
        assert!(validate_install_handoff_ini("[Unexpected]\nFormatPartition=false\n").is_err());
        assert!(validate_install_handoff_ini("[Install]\nFormatPartiton=false\n").is_err());
        assert!(validate_install_handoff_ini("[Install]\nformatpartition=false\n").is_err());
        assert!(validate_install_handoff_ini("[Instal]\nFormatPartiton=false\n").is_err());
    }

    #[test]
    fn historical_advanced_fields_under_install_remain_compatible() {
        validate_install_handoff_ini(
            "[Install]\nVolumeIndex=1\nDisableWindowsDefender=false\nWin7UefiPatch=true\n",
        )
        .unwrap();
    }

    #[test]
    fn canonical_target_round_trips_snapshot_identity() {
        let snapshot = gpt_snapshot(1, 2);
        let target =
            CanonicalInstallTargetV2::from_snapshot(&snapshot, 1_048_576, 500_000).unwrap();
        assert!(target.matches_snapshot(&snapshot));
        assert!(!target.matches_snapshot(&gpt_snapshot(1, 9)));
    }

    #[test]
    fn canonical_target_requires_one_unique_physical_disk() {
        let snapshot = gpt_snapshot(1, 2);
        let target =
            CanonicalInstallTargetV2::from_snapshot(&snapshot, 1_048_576, 500_000).unwrap();
        assert_eq!(
            unique_canonical_target_match(&target, &[(4, snapshot.clone())]).unwrap(),
            4
        );
        assert!(
            unique_canonical_target_match(&target, &[(4, snapshot.clone()), (7, snapshot)])
                .is_err()
        );
    }

    #[test]
    fn canonical_target_uses_storage_id_when_both_environments_expose_it() {
        let snapshot = gpt_snapshot(1, 2);
        let target =
            CanonicalInstallTargetV2::from_snapshot(&snapshot, 1_048_576, 500_000).unwrap();
        let mut unavailable_in_pe = snapshot.clone();
        unavailable_in_pe.device_id_hash = None;
        assert!(target.matches_snapshot(&unavailable_in_pe));

        let mut conflicting = snapshot.clone();
        conflicting.device_id_hash = Some([9; 32]);
        assert!(!target.matches_snapshot(&conflicting));
    }

    #[test]
    fn missing_storage_id_never_lets_an_ambiguous_clone_inventory_pass() {
        let snapshot = gpt_snapshot(1, 2);
        let target =
            CanonicalInstallTargetV2::from_snapshot(&snapshot, 1_048_576, 500_000).unwrap();
        let mut no_id = snapshot.clone();
        no_id.device_id_hash = None;
        assert!(unique_canonical_target_match(
            &target,
            &[(3, no_id.clone()), (7, snapshot.clone())]
        )
        .is_err());
        assert!(
            unique_canonical_target_match(&target, &[(3, no_id.clone()), (4, no_id)])
                .unwrap_err()
                .to_string()
                .contains("multiple cloned")
        );
    }

    #[test]
    fn canonical_handoff_corruption_is_rejected_strictly() {
        let valid = concat!(
            "[Install]\nVolumeIndex=1\nCanonicalTargetVersion=2\n",
            "CanonicalDiskLayoutSha256=1111111111111111111111111111111111111111111111111111111111111111\n",
            "CanonicalPartitionOffsetBytes=1048576\nCanonicalPartitionLengthBytes=500000\n",
            "CanonicalDiskStyle=GPT\nCanonicalGptPartitionId=22222222222222222222222222222222\n"
        );
        validate_install_handoff_ini(valid).unwrap();
        assert!(validate_install_handoff_ini(
            &valid.replace("CanonicalDiskStyle=GPT", "CanonicalDiskStyle=RAW")
        )
        .is_err());
        assert!(validate_install_handoff_ini(&valid.replace(
            "CanonicalGptPartitionId=22222222222222222222222222222222",
            "CanonicalGptPartitionId=broken"
        ))
        .is_err());
    }
}
