//! Versioned, drive-letter-independent backup authorization shared by normal Windows and WinPE.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::Digest;

use crate::install_handoff::{
    canonical_target_from_fields, encode_hex, unique_canonical_target_match,
    CanonicalInstallTargetV2, CANONICAL_TARGET_VERSION,
};
use crate::windows_storage::DiskLayoutSnapshot;

pub const BACKUP_HANDOFF_VERSION: u8 = 2;
pub const BACKUP_MARKER_MAGIC: &str = "LRBK2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupOutputPolicy {
    Create,
    Replace,
    Append,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupBaseFileIdentity {
    pub length_bytes: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupPayloadFields {
    pub save_path: String,
    pub name: String,
    pub description: String,
    pub source_partition: String,
    pub incremental: bool,
    pub format: u8,
    pub swm_split_size: u32,
    pub wim_engine: u8,
    pub language: String,
}

impl BackupOutputPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Replace => "replace",
            Self::Append => "append",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "create" => Ok(Self::Create),
            "replace" => Ok(Self::Replace),
            "append" => Ok(Self::Append),
            _ => bail!("invalid backup output policy: {value}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupHandoffV2 {
    pub session_id: String,
    pub source: CanonicalInstallTargetV2,
    pub destination: CanonicalInstallTargetV2,
    pub destination_relative_path: PathBuf,
    pub output_policy: BackupOutputPolicy,
    pub base_file: Option<BackupBaseFileIdentity>,
}

impl BackupHandoffV2 {
    pub fn validate(&self) -> Result<()> {
        validate_session_id(&self.session_id)?;
        validate_relative_output_path(&self.destination_relative_path)?;
        if self.source == self.destination {
            bail!("backup source and destination resolve to the same stable volume");
        }
        match (self.output_policy, &self.base_file) {
            (BackupOutputPolicy::Create, None) => {}
            (BackupOutputPolicy::Create, Some(_)) => {
                bail!("create backup authorization must not contain a base file")
            }
            (BackupOutputPolicy::Replace | BackupOutputPolicy::Append, Some(identity))
                if identity.length_bytes > 0 => {}
            (BackupOutputPolicy::Replace | BackupOutputPolicy::Append, Some(_)) => {
                bail!("replace/append backup base length must be non-zero")
            }
            (BackupOutputPolicy::Replace | BackupOutputPolicy::Append, None) => {
                bail!("replace/append backup authorization requires a base file identity")
            }
        }
        Ok(())
    }

    pub fn serialize_fields(&self) -> Result<String> {
        self.validate()?;
        let relative_path = self
            .destination_relative_path
            .to_str()
            .context("backup destination is not valid Unicode")?;
        let base = self
            .base_file
            .as_ref()
            .map(|identity| {
                format!(
                    "BaseLengthBytes={}\r\nBaseSha256={}\r\n",
                    identity.length_bytes,
                    encode_hex(&identity.sha256),
                )
            })
            .unwrap_or_default();
        Ok(format!(
            "HandoffVersion={}\r\nSessionId={}\r\nOutputPolicy={}\r\nDestinationRelativePath={}\r\n{}{}{}",
            BACKUP_HANDOFF_VERSION,
            self.session_id,
            self.output_policy.as_str(),
            relative_path,
            base,
            serialize_volume("Source", &self.source),
            serialize_volume("Destination", &self.destination),
        ))
    }

    pub fn parse_fields(content: &str) -> Result<Self> {
        let fields = parse_strict_backup_fields(content)?;
        parse_handoff_from_fields(&fields)
    }
}

pub fn parse_backup_payload(content: &str) -> Result<(BackupPayloadFields, BackupHandoffV2)> {
    if content.len() > 128 * 1024 {
        bail!("backup handoff exceeds its byte limit");
    }
    let fields = parse_strict_backup_fields(content)?;
    let value = |name: &str| -> Result<&str> {
        fields
            .get(name)
            .copied()
            .with_context(|| format!("missing backup handoff field {name}"))
    };
    let parse_bool = |name: &str| -> Result<bool> {
        match value(name)? {
            "true" => Ok(true),
            "false" => Ok(false),
            other => bail!("invalid {name} boolean: {other}"),
        }
    };
    let save_path = value("SavePath")?.to_owned();
    let name = value("Name")?.to_owned();
    let description = value("Description")?.to_owned();
    let source_partition = value("SourcePartition")?.to_owned();
    let language = value("Language")?.to_owned();
    validate_text(&save_path, 32_767, false, "SavePath")?;
    validate_text(&name, 256, false, "Name")?;
    validate_text(&description, 2_048, true, "Description")?;
    validate_text(&language, 32, false, "Language")?;
    if source_partition.len() != 2
        || !source_partition.as_bytes()[0].is_ascii_alphabetic()
        || source_partition.as_bytes()[1] != b':'
    {
        bail!("SourcePartition must be one local drive letter");
    }
    let incremental = parse_bool("Incremental")?;
    let format = value("Format")?
        .parse::<u8>()
        .context("invalid backup Format")?;
    if !matches!(format, 0 | 1) {
        bail!("LRBK2 currently supports only WIM/ESD format values 0 and 1");
    }
    let swm_split_size = value("SwmSplitSize")?
        .parse::<u32>()
        .context("invalid SwmSplitSize")?;
    if !(512..=8192).contains(&swm_split_size) {
        bail!("SwmSplitSize is outside the canonical range");
    }
    let wim_engine = value("WimEngine")?
        .parse::<u8>()
        .context("invalid WimEngine")?;
    if wim_engine > 1 {
        bail!("unsupported WimEngine");
    }
    let handoff = parse_handoff_from_fields(&fields)?;
    match (incremental, handoff.output_policy) {
        (true, BackupOutputPolicy::Append)
        | (false, BackupOutputPolicy::Create | BackupOutputPolicy::Replace) => {}
        _ => bail!("Incremental and OutputPolicy are inconsistent"),
    }
    Ok((
        BackupPayloadFields {
            save_path,
            name,
            description,
            source_partition,
            incremental,
            format,
            swm_split_size,
            wim_engine,
            language,
        },
        handoff,
    ))
}

fn validate_text(value: &str, maximum_utf16: usize, allow_empty: bool, field: &str) -> Result<()> {
    if (!allow_empty && value.is_empty())
        || value.encode_utf16().count() > maximum_utf16
        || value.chars().any(char::is_control)
    {
        bail!("invalid or oversized backup field {field}");
    }
    Ok(())
}

fn parse_handoff_from_fields(fields: &BTreeMap<&str, &str>) -> Result<BackupHandoffV2> {
    let value = |name: &str| -> Result<&str> {
        fields
            .get(name)
            .copied()
            .with_context(|| format!("missing backup handoff field {name}"))
    };
    let version = value("HandoffVersion")?
        .parse::<u8>()
        .context("invalid backup handoff version")?;
    if version != BACKUP_HANDOFF_VERSION {
        bail!("unsupported backup handoff version {version}");
    }
    let source = parse_volume("Source", fields)?;
    let destination = parse_volume("Destination", fields)?;
    let base_length = fields.get("BaseLengthBytes").copied();
    let base_sha256 = fields.get("BaseSha256").copied();
    let base_file = match (base_length, base_sha256) {
        (None, None) => None,
        (Some(length), Some(hash)) => Some(BackupBaseFileIdentity {
            length_bytes: length
                .parse::<u64>()
                .context("invalid backup base length")?,
            sha256: crate::install_handoff::decode_hex_array::<32>(hash, "BaseSha256")
                .context("invalid backup base SHA-256")?,
        }),
        _ => bail!("backup base identity fields must appear together"),
    };
    let handoff = BackupHandoffV2 {
        session_id: value("SessionId")?.to_string(),
        source,
        destination,
        destination_relative_path: PathBuf::from(value("DestinationRelativePath")?),
        output_policy: BackupOutputPolicy::parse(value("OutputPolicy")?)?,
        base_file,
    };
    handoff.validate()?;
    Ok(handoff)
}

fn serialize_volume(prefix: &str, value: &CanonicalInstallTargetV2) -> String {
    format!(
        "{prefix}CanonicalVersion={}\r\n{prefix}LayoutSha256={}\r\n{}{prefix}OffsetBytes={}\r\n{prefix}LengthBytes={}\r\n{prefix}DiskStyle={}\r\n{prefix}GptPartitionId={}\r\n",
        CANONICAL_TARGET_VERSION,
        encode_hex(&value.layout_digest),
        value.device_id_hash.map(|hash| format!(
            "{prefix}StorageIdSha256={}\r\n",
            encode_hex(&hash)
        )).unwrap_or_default(),
        value.partition_offset_bytes,
        value.partition_length_bytes,
        value.style.as_str(),
        value.gpt_partition_id.map(|id| encode_hex(&id)).unwrap_or_else(|| "none".into()),
    )
}

fn parse_volume(prefix: &str, fields: &BTreeMap<&str, &str>) -> Result<CanonicalInstallTargetV2> {
    let value = |name: &str| -> Result<&str> {
        fields
            .get(name)
            .copied()
            .with_context(|| format!("missing backup handoff field {name}"))
    };
    let version = value(&format!("{prefix}CanonicalVersion"))?.parse::<u8>()?;
    let offset = value(&format!("{prefix}OffsetBytes"))?.parse::<u64>()?;
    let length = value(&format!("{prefix}LengthBytes"))?.parse::<u64>()?;
    let storage_key = format!("{prefix}StorageIdSha256");
    let storage = fields.get(storage_key.as_str()).copied();
    canonical_target_from_fields(
        Some(version),
        Some(value(&format!("{prefix}LayoutSha256"))?),
        Some(offset),
        Some(length),
        Some(value(&format!("{prefix}DiskStyle"))?),
        Some(value(&format!("{prefix}GptPartitionId"))?),
        storage,
    )?
    .context("backup handoff canonical volume is absent")
}

fn parse_strict_backup_fields(content: &str) -> Result<BTreeMap<&str, &str>> {
    const REQUIRED: &[&str] = &[
        "SavePath",
        "Name",
        "Description",
        "SourcePartition",
        "Incremental",
        "Format",
        "SwmSplitSize",
        "WimEngine",
        "Language",
        "HandoffVersion",
        "SessionId",
        "OutputPolicy",
        "DestinationRelativePath",
        "SourceCanonicalVersion",
        "SourceLayoutSha256",
        "SourceOffsetBytes",
        "SourceLengthBytes",
        "SourceDiskStyle",
        "SourceGptPartitionId",
        "DestinationCanonicalVersion",
        "DestinationLayoutSha256",
        "DestinationOffsetBytes",
        "DestinationLengthBytes",
        "DestinationDiskStyle",
        "DestinationGptPartitionId",
    ];
    const OPTIONAL: &[&str] = &[
        "SourceStorageIdSha256",
        "DestinationStorageIdSha256",
        "BaseLengthBytes",
        "BaseSha256",
        "HandoffManifestVersion",
        "HandoffManifestLength",
        "HandoffManifestSha256",
    ];

    let allowed = REQUIRED
        .iter()
        .chain(OPTIONAL.iter())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut fields = BTreeMap::new();
    let mut saw_backup_section = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if line != "[Backup]" || saw_backup_section {
                bail!("unknown or duplicate backup handoff section: {line}");
            }
            saw_backup_section = true;
            continue;
        }
        let (key, raw_value) = line
            .split_once('=')
            .with_context(|| format!("invalid backup handoff line: {line}"))?;
        let key = key.trim();
        let value = raw_value.trim();
        if !allowed.contains(key) {
            bail!("unknown backup handoff field {key}");
        }
        if fields.insert(key, value).is_some() {
            bail!("duplicate backup handoff field {key}");
        }
    }
    if !saw_backup_section {
        bail!("missing [Backup] section");
    }
    for key in REQUIRED {
        if !fields.contains_key(key) {
            bail!("missing backup handoff field {key}");
        }
    }
    Ok(fields)
}

pub fn validate_session_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        bail!("invalid backup SessionId");
    }
    Ok(())
}

pub fn validate_relative_output_path(path: &Path) -> Result<()> {
    let mut components = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value
                    .to_str()
                    .context("backup destination contains invalid Unicode")?;
                validate_windows_file_component(value)?;
                components += 1;
            }
            _ => bail!("backup destination must be a strict relative path"),
        }
    }
    if components == 0 || path.file_name().is_none() {
        bail!("backup destination relative path has no file name");
    }
    Ok(())
}

fn validate_windows_file_component(value: &str) -> Result<()> {
    if value.is_empty()
        || value.ends_with([' ', '.'])
        || value.encode_utf16().count() > 255
        || value.chars().any(|character| {
            character <= '\u{1f}'
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '='
                )
        })
    {
        bail!("backup destination contains an invalid Windows path component");
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CLOCK$"
            | "CONIN$"
            | "CONOUT$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    );
    if reserved {
        bail!("backup destination uses a reserved DOS device name");
    }
    Ok(())
}

pub fn bind_unique_backup_volumes(
    handoff: &BackupHandoffV2,
    candidates: &[(u32, DiskLayoutSnapshot)],
) -> Result<(u32, u32)> {
    handoff.validate()?;
    let source_disk = unique_canonical_target_match(&handoff.source, candidates)
        .context("rebind backup source")?;
    let destination_disk = unique_canonical_target_match(&handoff.destination, candidates)
        .context("rebind backup destination")?;
    if source_disk == destination_disk
        && handoff.source.partition_offset_bytes == handoff.destination.partition_offset_bytes
    {
        bail!("backup source and destination rebound to the same physical volume");
    }
    Ok((source_disk, destination_disk))
}

pub fn marker_text(session_id: &str, config_bytes: &[u8]) -> Result<String> {
    validate_session_id(session_id)?;
    let digest = sha2::Sha256::digest(config_bytes);
    Ok(format!(
        "{BACKUP_MARKER_MAGIC}\r\nSessionId={session_id}\r\nConfigSha256={}\r\n",
        encode_hex(&digest)
    ))
}

pub fn validate_marker(marker: &str, handoff: &BackupHandoffV2, config_bytes: &[u8]) -> Result<()> {
    if marker.lines().next().map(str::trim) != Some(BACKUP_MARKER_MAGIC) {
        bail!("invalid backup marker version");
    }
    let expected = marker_text(&handoff.session_id, config_bytes)?;
    if marker.replace('\n', "\r\n").replace("\r\r\n", "\r\n") != expected {
        bail!("backup marker does not match the exact session/configuration");
    }
    Ok(())
}

pub fn marker_session_id(marker: &str) -> Result<&str> {
    let mut lines = marker.lines().map(str::trim);
    if lines.next() != Some(BACKUP_MARKER_MAGIC) {
        bail!("invalid backup marker version");
    }
    let session = lines
        .next()
        .and_then(|line| line.strip_prefix("SessionId="))
        .context("backup marker is missing SessionId")?;
    validate_session_id(session)?;
    let hash = lines
        .next()
        .and_then(|line| line.strip_prefix("ConfigSha256="))
        .context("backup marker is missing ConfigSha256")?;
    let _ = crate::install_handoff::decode_hex_array::<32>(hash, "ConfigSha256")?;
    if lines.next().is_some() {
        bail!("backup marker has trailing fields");
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_handoff::{CanonicalInstallTargetV2, CanonicalTargetStyle};

    fn volume(byte: u8, offset: u64) -> CanonicalInstallTargetV2 {
        CanonicalInstallTargetV2 {
            layout_digest: [byte; 32],
            device_id_hash: Some([byte.wrapping_add(1); 32]),
            partition_offset_bytes: offset,
            partition_length_bytes: 1024,
            style: CanonicalTargetStyle::Gpt,
            gpt_partition_id: Some([byte; 16]),
        }
    }

    fn handoff() -> BackupHandoffV2 {
        BackupHandoffV2 {
            session_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            source: volume(1, 1024),
            destination: volume(2, 2048),
            destination_relative_path: PathBuf::from("images/system.wim"),
            output_policy: BackupOutputPolicy::Append,
            base_file: Some(BackupBaseFileIdentity {
                length_bytes: 4096,
                sha256: [9; 32],
            }),
        }
    }

    fn serialized_config(value: &BackupHandoffV2) -> String {
        format!(
            "[Backup]\r\nSavePath=D:\\images\\system.wim\r\nName=System\r\nDescription=\r\nSourcePartition=C:\r\nIncremental=true\r\nFormat=0\r\nSwmSplitSize=4096\r\nWimEngine=0\r\nLanguage=zh-CN\r\n{}",
            value.serialize_fields().unwrap()
        )
    }

    #[test]
    fn round_trip_preserves_stable_volumes_and_policy() {
        let expected = handoff();
        let text = serialized_config(&expected);
        assert_eq!(BackupHandoffV2::parse_fields(&text).unwrap(), expected);
    }

    #[test]
    fn traversal_and_stale_marker_are_rejected() {
        let mut invalid = handoff();
        invalid.destination_relative_path = PathBuf::from("../system.wim");
        assert!(invalid.validate().is_err());
        let valid = handoff();
        let marker = marker_text(&valid.session_id, b"one").unwrap();
        assert!(validate_marker(&marker, &valid, b"two").is_err());
    }

    #[test]
    fn rejects_unknown_duplicate_and_unsafe_windows_paths() {
        let valid = handoff();
        let text = serialized_config(&valid);
        assert!(BackupHandoffV2::parse_fields(&format!("{text}Unexpected=true\r\n")).is_err());
        assert!(BackupHandoffV2::parse_fields(&format!("{text}Format=1\r\n")).is_err());
        for path in [
            "images/file.wim:stream",
            "images/CON.wim",
            "images/CONOUT$.log",
            "images/COM¹.wim",
            "images/trailing. ",
            "images/ques?.wim",
        ] {
            let mut invalid = valid.clone();
            invalid.destination_relative_path = PathBuf::from(path);
            assert!(invalid.validate().is_err(), "accepted unsafe path {path}");
        }

        let mut zero_base = valid.clone();
        zero_base.base_file.as_mut().unwrap().length_bytes = 0;
        assert!(zero_base.validate().is_err());
        let mut missing_base = valid.clone();
        missing_base.base_file = None;
        assert!(missing_base.validate().is_err());
        let mut create_with_base = valid;
        create_with_base.output_policy = BackupOutputPolicy::Create;
        assert!(create_with_base.validate().is_err());

        assert!(
            parse_backup_payload(&text.replace("Incremental=true", "Incremental=yes")).is_err()
        );
        assert!(parse_backup_payload(&text.replace("Format=0", "Format=3")).is_err());
        assert!(parse_backup_payload(&text.replace("WimEngine=0", "WimEngine=9")).is_err());
        assert!(
            parse_backup_payload(&text.replace("Incremental=true", "Incremental=false")).is_err()
        );
    }
}
