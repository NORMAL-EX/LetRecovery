//! Strict manifest for files and partition-cleanup authority referenced by a cross-reboot task.
//!
//! The manifest is public. Its exact length and SHA-256 must be embedded in the configuration
//! authenticated by `handoff_auth`; a manifest digest alone is not an authorization boundary.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::handoff_auth::{validate_locator_token, validate_session_id, HandoffPurpose};
use crate::install_handoff::{
    decode_hex_array, encode_hex, CanonicalInstallTargetV2, CanonicalTargetStyle,
};

pub const HANDOFF_MANIFEST_MAGIC: &str = "LRHM3";
pub const HANDOFF_MANIFEST_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const HANDOFF_MANIFEST_MAX_ARTIFACTS: usize = 4096;
pub const HANDOFF_ARTIFACT_MAX_PATH_BYTES: usize = 4096;
pub const HANDOFF_ARTIFACT_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ArtifactRole {
    InstallImageSpan,
    XpSourceFile,
    CustomUnattend,
    XpAnswer,
    PreservedDriver,
    UserDriver,
    StorageControllerDriver,
    DeployScript,
    FirstLoginScript,
    RegistryImport,
    CustomFile,
    PcaPackage,
    UefiSevenFile,
    UpdatePackage,
    Win7DriverPackage,
    PreinstalledSoftware,
    AutoPartitionMarker,
    ProtectedAdministratorSecret,
    ProtectedBitLockerSecret,
    BackupBaseImage,
}

impl ArtifactRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InstallImageSpan => "install_image_span",
            Self::XpSourceFile => "xp_source_file",
            Self::CustomUnattend => "custom_unattend",
            Self::XpAnswer => "xp_answer",
            Self::PreservedDriver => "preserved_driver",
            Self::UserDriver => "user_driver",
            Self::StorageControllerDriver => "storage_controller_driver",
            Self::DeployScript => "deploy_script",
            Self::FirstLoginScript => "first_login_script",
            Self::RegistryImport => "registry_import",
            Self::CustomFile => "custom_file",
            Self::PcaPackage => "pca_package",
            Self::UefiSevenFile => "uefiseven_file",
            Self::UpdatePackage => "update_package",
            Self::Win7DriverPackage => "win7_driver_package",
            Self::PreinstalledSoftware => "preinstalled_software",
            Self::AutoPartitionMarker => "auto_partition_marker",
            Self::ProtectedAdministratorSecret => "protected_administrator_secret",
            Self::ProtectedBitLockerSecret => "protected_bitlocker_secret",
            Self::BackupBaseImage => "backup_base_image",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "install_image_span" => Ok(Self::InstallImageSpan),
            "xp_source_file" => Ok(Self::XpSourceFile),
            "custom_unattend" => Ok(Self::CustomUnattend),
            "xp_answer" => Ok(Self::XpAnswer),
            "preserved_driver" => Ok(Self::PreservedDriver),
            "user_driver" => Ok(Self::UserDriver),
            "storage_controller_driver" => Ok(Self::StorageControllerDriver),
            "deploy_script" => Ok(Self::DeployScript),
            "first_login_script" => Ok(Self::FirstLoginScript),
            "registry_import" => Ok(Self::RegistryImport),
            "custom_file" => Ok(Self::CustomFile),
            "pca_package" => Ok(Self::PcaPackage),
            "uefiseven_file" => Ok(Self::UefiSevenFile),
            "update_package" => Ok(Self::UpdatePackage),
            "win7_driver_package" => Ok(Self::Win7DriverPackage),
            "preinstalled_software" => Ok(Self::PreinstalledSoftware),
            "auto_partition_marker" => Ok(Self::AutoPartitionMarker),
            "protected_administrator_secret" => Ok(Self::ProtectedAdministratorSecret),
            "protected_bitlocker_secret" => Ok(Self::ProtectedBitLockerSecret),
            "backup_base_image" => Ok(Self::BackupBaseImage),
            _ => bail!("unknown handoff artifact role"),
        }
    }

    const fn requires_protected_boot(self) -> bool {
        matches!(
            self,
            Self::CustomUnattend
                | Self::XpAnswer
                | Self::DeployScript
                | Self::FirstLoginScript
                | Self::RegistryImport
                | Self::CustomFile
                | Self::ProtectedAdministratorSecret
                | Self::ProtectedBitLockerSecret
        )
    }

    /// Roles whose object is itself the payload must never authenticate an empty file. Driver and
    /// XP source trees deliberately remain outside this set because a valid package tree may
    /// contain empty ordinary members bound by SHA-256(empty).
    const fn requires_nonempty_payload(self) -> bool {
        matches!(
            self,
            Self::InstallImageSpan
                | Self::PcaPackage
                | Self::AutoPartitionMarker
                | Self::ProtectedAdministratorSecret
                | Self::ProtectedBitLockerSecret
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ArtifactLocation {
    PublicData,
    ProtectedBoot,
}

impl ArtifactLocation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PublicData => "public_data",
            Self::ProtectedBoot => "protected_boot",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "public_data" => Ok(Self::PublicData),
            "protected_boot" => Ok(Self::ProtectedBoot),
            _ => bail!("unknown handoff artifact location"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRecord {
    pub role: ArtifactRole,
    pub location: ArtifactLocation,
    pub ordinal: u32,
    pub relative_path: String,
    pub length_bytes: u64,
    pub sha256: [u8; 32],
}

impl ArtifactRecord {
    fn validate(&self) -> Result<()> {
        validate_relative_path(&self.relative_path)?;
        // Empty ordinary files are valid members of driver and XP source trees and still have a
        // stable, nonzero SHA-256 identity. Payload-object roles are rejected here so PE cannot
        // cross a destructive boundary with a semantically empty image, marker, package or secret.
        if self.length_bytes == 0 && self.role.requires_nonempty_payload() {
            bail!("handoff artifact role requires a nonempty payload");
        }
        if self.length_bytes > HANDOFF_ARTIFACT_MAX_BYTES {
            bail!("handoff artifact length is outside its limit");
        }
        if self.sha256 == [0; 32] {
            bail!("handoff artifact SHA-256 must not be all-zero");
        }
        if self.role.requires_protected_boot() && self.location != ArtifactLocation::ProtectedBoot {
            bail!("secret-bearing handoff artifact must be in the protected boot payload");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoStagingAuthorization {
    pub source: CanonicalInstallTargetV2,
    pub temporary: CanonicalInstallTargetV2,
    /// Exact source length observed before the normal endpoint asked VDS to shrink it.
    ///
    /// Providers may shrink by slightly more than the created partition length and may leave
    /// legal free gaps on either side of that partition. PE must restore this authenticated
    /// original boundary instead of guessing from the temporary partition geometry.
    pub source_length_before_bytes: u64,
}

impl AutoStagingAuthorization {
    pub fn reclaim_length_bytes(&self) -> Result<u64> {
        validate_extent(&self.source, "source")?;
        validate_extent(&self.temporary, "temporary")?;
        if self.source.layout_digest != self.temporary.layout_digest {
            bail!("automatic staging extents must belong to the same canonical disk layout");
        }
        if self.source.device_id_hash != self.temporary.device_id_hash {
            bail!("automatic staging extents do not share the same captured storage identity");
        }
        let source_end = self
            .source
            .partition_offset_bytes
            .checked_add(self.source.partition_length_bytes)
            .context("automatic staging source extent end overflows")?;
        if source_end > self.temporary.partition_offset_bytes {
            bail!("automatic staging extent overlaps or precedes the source extent");
        }
        let temporary_end = self
            .temporary
            .partition_offset_bytes
            .checked_add(self.temporary.partition_length_bytes)
            .context("automatic staging temporary extent end overflows")?;
        let original_source_end = self
            .source
            .partition_offset_bytes
            .checked_add(self.source_length_before_bytes)
            .context("automatic staging original source extent end overflows")?;
        if self.source_length_before_bytes <= self.source.partition_length_bytes {
            bail!("automatic staging original source length must exceed its post-shrink length");
        }
        if temporary_end > original_source_end {
            bail!("automatic staging extent exceeds the authenticated original source boundary");
        }
        self.source_length_before_bytes
            .checked_sub(self.source.partition_length_bytes)
            .context("automatic staging reclaim length underflows")
    }

    fn validate(&self) -> Result<()> {
        self.reclaim_length_bytes().map(|_| ())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffManifest {
    pub purpose: HandoffPurpose,
    pub session_id: String,
    pub data_locator_token: String,
    pub install_target_token: Option<String>,
    pub auto_staging: Option<AutoStagingAuthorization>,
    pub artifacts: Vec<ArtifactRecord>,
}

impl HandoffManifest {
    pub fn new(
        purpose: HandoffPurpose,
        session_id: impl Into<String>,
        data_locator_token: impl Into<String>,
        install_target_token: Option<String>,
        auto_staging: Option<AutoStagingAuthorization>,
        mut artifacts: Vec<ArtifactRecord>,
    ) -> Result<Self> {
        artifacts.sort_by_key(artifact_key);
        let manifest = Self {
            purpose,
            session_id: session_id.into(),
            data_locator_token: data_locator_token.into(),
            install_target_token,
            auto_staging,
            artifacts,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        validate_session_id(&self.session_id)?;
        validate_locator_token(&self.data_locator_token)?;
        match (self.purpose, self.install_target_token.as_deref()) {
            (HandoffPurpose::Install, Some(token)) => {
                validate_locator_token(token)?;
                if token == self.data_locator_token {
                    bail!("installation data and target locator tokens must be independent");
                }
            }
            (HandoffPurpose::Install, None) => {
                bail!("installation handoff has no target locator token")
            }
            (_, Some(_)) => bail!("only installation handoffs may carry a target locator token"),
            (_, None) => {}
        }
        if self.artifacts.len() > HANDOFF_MANIFEST_MAX_ARTIFACTS {
            bail!("handoff manifest contains too many artifacts");
        }
        if let Some(staging) = &self.auto_staging {
            if self.purpose != HandoffPurpose::Install {
                bail!("automatic staging deletion is valid only for installation handoffs");
            }
            staging.validate()?;
        }
        let mut total_bytes = 0_u64;
        let mut paths: Vec<(ArtifactLocation, &str)> = Vec::with_capacity(self.artifacts.len());
        let mut roles: BTreeMap<ArtifactRole, Vec<&ArtifactRecord>> = BTreeMap::new();
        let mut previous = None;
        for artifact in &self.artifacts {
            artifact.validate()?;
            total_bytes = total_bytes
                .checked_add(artifact.length_bytes)
                .context("handoff artifact total length overflow")?;
            if total_bytes > HANDOFF_ARTIFACT_MAX_BYTES {
                bail!("handoff artifact total length exceeds its limit");
            }
            let key = artifact_key(artifact);
            if previous.as_ref().is_some_and(|value| value >= &key) {
                bail!("handoff artifacts are not in strict canonical order");
            }
            previous = Some(key);
            for (location, path) in &paths {
                if *location == artifact.location
                    && paths_equal_ignore_case(path, &artifact.relative_path)?
                {
                    bail!("handoff manifest contains a duplicate case-insensitive path");
                }
            }
            paths.push((artifact.location, &artifact.relative_path));
            roles.entry(artifact.role).or_default().push(artifact);
        }
        validate_role_matrix(self.purpose, self.auto_staging.is_some(), &roles)?;
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut text = String::new();
        text.push_str(HANDOFF_MANIFEST_MAGIC);
        text.push_str("\r\n");
        push_line(&mut text, "Purpose", self.purpose.as_str())?;
        push_line(&mut text, "SessionId", &self.session_id)?;
        push_line(&mut text, "DataLocatorToken", &self.data_locator_token)?;
        push_line(
            &mut text,
            "InstallTargetToken",
            self.install_target_token.as_deref().unwrap_or("none"),
        )?;
        push_line(
            &mut text,
            "AutoStagingPresent",
            if self.auto_staging.is_some() {
                "true"
            } else {
                "false"
            },
        )?;
        if let Some(staging) = &self.auto_staging {
            push_extent(&mut text, "Source", &staging.source)?;
            push_extent(&mut text, "Temporary", &staging.temporary)?;
            push_line(
                &mut text,
                "SourceLengthBeforeBytes",
                &staging.source_length_before_bytes.to_string(),
            )?;
        }
        push_line(
            &mut text,
            "ArtifactCount",
            &self.artifacts.len().to_string(),
        )?;
        for (index, artifact) in self.artifacts.iter().enumerate() {
            let prefix = format!("Artifact{index:04}");
            push_line(&mut text, &format!("{prefix}.Role"), artifact.role.as_str())?;
            push_line(
                &mut text,
                &format!("{prefix}.Location"),
                artifact.location.as_str(),
            )?;
            push_line(
                &mut text,
                &format!("{prefix}.Ordinal"),
                &artifact.ordinal.to_string(),
            )?;
            push_line(
                &mut text,
                &format!("{prefix}.PathUtf8Hex"),
                &encode_hex(artifact.relative_path.as_bytes()),
            )?;
            push_line(
                &mut text,
                &format!("{prefix}.Length"),
                &artifact.length_bytes.to_string(),
            )?;
            push_line(
                &mut text,
                &format!("{prefix}.Sha256"),
                &encode_hex(&artifact.sha256),
            )?;
        }
        if text.len() > HANDOFF_MANIFEST_MAX_BYTES {
            bail!("handoff manifest exceeds its byte limit");
        }
        Ok(text.into_bytes())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > HANDOFF_MANIFEST_MAX_BYTES {
            bail!("handoff manifest length is outside its limit");
        }
        let text = std::str::from_utf8(bytes).context("handoff manifest is not UTF-8")?;
        if text.starts_with('\u{feff}') || text.replace("\r\n", "").contains(['\r', '\n']) {
            bail!("handoff manifest has invalid line endings");
        }
        let mut lines = text.split("\r\n");
        if lines.next() != Some(HANDOFF_MANIFEST_MAGIC) {
            bail!("unsupported or legacy handoff manifest");
        }
        let purpose = HandoffPurpose::parse(exact_line(&mut lines, "Purpose")?)?;
        let session_id = exact_line(&mut lines, "SessionId")?.to_owned();
        let data_locator_token = exact_line(&mut lines, "DataLocatorToken")?.to_owned();
        let install_target_token = match exact_line(&mut lines, "InstallTargetToken")? {
            "none" => None,
            token => Some(token.to_owned()),
        };
        let auto_staging = match exact_line(&mut lines, "AutoStagingPresent")? {
            "false" => None,
            "true" => Some(AutoStagingAuthorization {
                source: parse_extent(&mut lines, "Source")?,
                temporary: parse_extent(&mut lines, "Temporary")?,
                source_length_before_bytes: parse_canonical_u64(
                    exact_line(&mut lines, "SourceLengthBeforeBytes")?,
                    "SourceLengthBeforeBytes",
                )?,
            }),
            _ => bail!("invalid AutoStagingPresent value"),
        };
        let count = parse_canonical_u64(exact_line(&mut lines, "ArtifactCount")?, "ArtifactCount")?;
        let count = usize::try_from(count).context("artifact count does not fit usize")?;
        if count > HANDOFF_MANIFEST_MAX_ARTIFACTS {
            bail!("handoff manifest contains too many artifacts");
        }
        let mut artifacts = Vec::with_capacity(count);
        for index in 0..count {
            let prefix = format!("Artifact{index:04}");
            let role = ArtifactRole::parse(exact_line(&mut lines, &format!("{prefix}.Role"))?)?;
            let location =
                ArtifactLocation::parse(exact_line(&mut lines, &format!("{prefix}.Location"))?)?;
            let ordinal = u32::try_from(parse_canonical_u64(
                exact_line(&mut lines, &format!("{prefix}.Ordinal"))?,
                "artifact ordinal",
            )?)
            .context("artifact ordinal exceeds u32")?;
            let path_hex = exact_line(&mut lines, &format!("{prefix}.PathUtf8Hex"))?;
            if path_hex.len() > HANDOFF_ARTIFACT_MAX_PATH_BYTES * 2
                || path_hex.len() % 2 != 0
                || !path_hex.bytes().all(|value| value.is_ascii_hexdigit())
            {
                bail!("artifact path hex is invalid");
            }
            let mut path_bytes = Vec::with_capacity(path_hex.len() / 2);
            for pair in path_hex.as_bytes().chunks_exact(2) {
                path_bytes.push((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?);
            }
            let relative_path =
                String::from_utf8(path_bytes).context("artifact path is not UTF-8")?;
            let length_bytes = parse_canonical_u64(
                exact_line(&mut lines, &format!("{prefix}.Length"))?,
                "artifact length",
            )?;
            let sha256 = decode_hex_array::<32>(
                exact_line(&mut lines, &format!("{prefix}.Sha256"))?,
                "artifact SHA-256",
            )?;
            artifacts.push(ArtifactRecord {
                role,
                location,
                ordinal,
                relative_path,
                length_bytes,
                sha256,
            });
        }
        if lines.any(|line| !line.is_empty()) {
            bail!("handoff manifest has trailing fields");
        }
        let manifest = Self {
            purpose,
            session_id,
            data_locator_token,
            install_target_token,
            auto_staging,
            artifacts,
        };
        manifest.validate()?;
        if manifest.to_bytes()? != bytes {
            bail!("handoff manifest is not in canonical byte form");
        }
        Ok(manifest)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestBinding {
    pub length_bytes: u64,
    pub sha256: [u8; 32],
}

impl ManifestBinding {
    pub fn new(manifest_bytes: &[u8]) -> Result<Self> {
        HandoffManifest::parse(manifest_bytes)?;
        Ok(Self {
            length_bytes: manifest_bytes.len() as u64,
            sha256: Sha256::digest(manifest_bytes).into(),
        })
    }

    pub fn verify(&self, manifest_bytes: &[u8]) -> Result<HandoffManifest> {
        if self.length_bytes != manifest_bytes.len() as u64 {
            bail!("handoff manifest length does not match authenticated binding");
        }
        let actual: [u8; 32] = Sha256::digest(manifest_bytes).into();
        if actual.ct_eq(&self.sha256).unwrap_u8() != 1 {
            bail!("handoff manifest SHA-256 does not match authenticated binding");
        }
        HandoffManifest::parse(manifest_bytes)
    }

    pub fn verify_for(
        &self,
        manifest_bytes: &[u8],
        expected_purpose: HandoffPurpose,
        expected_session_id: &str,
    ) -> Result<HandoffManifest> {
        validate_session_id(expected_session_id)?;
        let manifest = self.verify(manifest_bytes)?;
        if manifest.purpose != expected_purpose || manifest.session_id != expected_session_id {
            bail!("handoff manifest purpose/session does not match authenticated config");
        }
        Ok(manifest)
    }

    pub fn from_config_fields(version: &str, length: &str, sha256: &str) -> Result<Self> {
        if version != "1" {
            bail!("unsupported handoff manifest binding version");
        }
        let length_bytes = parse_canonical_u64(length, "handoff manifest length")?;
        if length_bytes == 0 || length_bytes > HANDOFF_MANIFEST_MAX_BYTES as u64 {
            bail!("handoff manifest binding length is outside its limit");
        }
        Ok(Self {
            length_bytes,
            sha256: decode_hex_array::<32>(sha256, "handoff manifest SHA-256")?,
        })
    }

    pub fn to_config_lines(self) -> String {
        format!(
            "HandoffManifestVersion=1\r\nHandoffManifestLength={}\r\nHandoffManifestSha256={}\r\n",
            self.length_bytes,
            encode_hex(&self.sha256)
        )
    }

    /// Extract the unique LRHM3 binding from authoritative configuration bytes. The surrounding
    /// configuration remains endpoint-specific, but duplicate, missing, uppercase or
    /// non-canonical binding fields are always rejected before any manifest path is opened.
    pub fn from_config_bytes(config_bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(config_bytes)
            .context("authoritative handoff configuration is not UTF-8")?;
        if text.starts_with('\u{feff}') || text.contains('\0') {
            bail!("authoritative handoff configuration has an invalid encoding prefix");
        }
        let mut version = None;
        let mut length = None;
        let mut sha256 = None;
        for raw_line in text.split('\n') {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            for (name, slot) in [
                ("HandoffManifestVersion", &mut version),
                ("HandoffManifestLength", &mut length),
                ("HandoffManifestSha256", &mut sha256),
            ] {
                if let Some(value) = line.strip_prefix(&format!("{name}=")) {
                    if slot.replace(value).is_some() {
                        bail!("authoritative handoff configuration repeats {name}");
                    }
                }
            }
        }
        if version != Some("1") {
            bail!("authoritative handoff configuration has no supported manifest version");
        }
        let length_bytes = parse_canonical_u64(
            length.context("authoritative handoff configuration is missing manifest length")?,
            "manifest binding length",
        )?;
        if length_bytes == 0 || length_bytes > HANDOFF_MANIFEST_MAX_BYTES as u64 {
            bail!("manifest binding length is outside its limit");
        }
        let sha256 =
            sha256.context("authoritative handoff configuration is missing manifest SHA-256")?;
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("manifest binding SHA-256 is not canonical lowercase hexadecimal");
        }
        Ok(Self {
            length_bytes,
            sha256: decode_hex_array::<32>(sha256, "manifest binding SHA-256")?,
        })
    }
}

fn validate_extent(value: &CanonicalInstallTargetV2, label: &str) -> Result<()> {
    if value.layout_digest == [0; 32] || value.partition_length_bytes == 0 {
        bail!("canonical {label} extent is incomplete");
    }
    match value.style {
        CanonicalTargetStyle::Gpt if value.gpt_partition_id.is_none() => {
            bail!("canonical GPT {label} extent has no partition identifier")
        }
        CanonicalTargetStyle::Mbr if value.gpt_partition_id.is_some() => {
            bail!("canonical MBR {label} extent unexpectedly has a GPT identifier")
        }
        _ => Ok(()),
    }
}

fn validate_role_matrix(
    purpose: HandoffPurpose,
    has_auto_staging: bool,
    roles: &BTreeMap<ArtifactRole, Vec<&ArtifactRecord>>,
) -> Result<()> {
    for (role, records) in roles {
        let allowed = match purpose {
            HandoffPurpose::Install => !matches!(
                role,
                ArtifactRole::BackupBaseImage | ArtifactRole::ProtectedBitLockerSecret
            ),
            HandoffPurpose::Backup => *role == ArtifactRole::BackupBaseImage,
            HandoffPurpose::Expand => false,
            HandoffPurpose::Maintenance => *role == ArtifactRole::ProtectedBitLockerSecret,
        };
        if !allowed {
            bail!("handoff artifact role is not allowed for this operation purpose");
        }
        for (expected, record) in records.iter().enumerate() {
            if record.ordinal != expected as u32 {
                bail!("handoff artifact ordinals must be unique and contiguous from zero per role");
            }
        }
        let singleton = matches!(
            role,
            ArtifactRole::CustomUnattend
                | ArtifactRole::XpAnswer
                | ArtifactRole::DeployScript
                | ArtifactRole::FirstLoginScript
                | ArtifactRole::RegistryImport
                | ArtifactRole::AutoPartitionMarker
                | ArtifactRole::ProtectedAdministratorSecret
                | ArtifactRole::ProtectedBitLockerSecret
                | ArtifactRole::BackupBaseImage
        );
        if singleton && records.len() != 1 {
            bail!("singleton handoff artifact role appears more than once");
        }
    }

    let staging_markers = roles
        .get(&ArtifactRole::AutoPartitionMarker)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if has_auto_staging != (staging_markers.len() == 1) {
        bail!("automatic staging authorization and marker artifact must be present together");
    }
    if let Some(marker) = staging_markers.first() {
        if marker.location != ArtifactLocation::PublicData || marker.ordinal != 0 {
            bail!("automatic staging marker must be the sole ordinal-zero public artifact");
        }
    }

    if purpose == HandoffPurpose::Install {
        let image_count = roles
            .get(&ArtifactRole::InstallImageSpan)
            .map_or(0, Vec::len);
        let xp_count = roles.get(&ArtifactRole::XpSourceFile).map_or(0, Vec::len);
        if image_count == 0 && xp_count == 0 {
            bail!("installation handoff manifest has no authenticated image or XP source files");
        }
        if image_count != 0 && xp_count != 0 {
            bail!("installation handoff cannot mix image spans with an XP source tree");
        }
    }
    if purpose == HandoffPurpose::Maintenance {
        let secret_count = roles
            .get(&ArtifactRole::ProtectedBitLockerSecret)
            .map_or(0, Vec::len);
        if secret_count > 1 {
            bail!("maintenance handoff has more than one protected BitLocker secret artifact");
        }
    }
    Ok(())
}

fn push_extent(text: &mut String, prefix: &str, value: &CanonicalInstallTargetV2) -> Result<()> {
    validate_extent(value, prefix)?;
    push_line(
        text,
        &format!("{prefix}.LayoutSha256"),
        &encode_hex(&value.layout_digest),
    )?;
    push_line(
        text,
        &format!("{prefix}.StorageIdSha256"),
        &value
            .device_id_hash
            .map(|value| encode_hex(&value))
            .unwrap_or_else(|| "none".to_owned()),
    )?;
    push_line(
        text,
        &format!("{prefix}.Offset"),
        &value.partition_offset_bytes.to_string(),
    )?;
    push_line(
        text,
        &format!("{prefix}.Length"),
        &value.partition_length_bytes.to_string(),
    )?;
    push_line(text, &format!("{prefix}.Style"), value.style.as_str())?;
    push_line(
        text,
        &format!("{prefix}.GptPartitionId"),
        &value
            .gpt_partition_id
            .map(|value| encode_hex(&value))
            .unwrap_or_else(|| "none".to_owned()),
    )
}

fn parse_extent<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    prefix: &str,
) -> Result<CanonicalInstallTargetV2> {
    let layout_digest = decode_hex_array::<32>(
        exact_line(lines, &format!("{prefix}.LayoutSha256"))?,
        "layout SHA-256",
    )?;
    let device = exact_line(lines, &format!("{prefix}.StorageIdSha256"))?;
    let device_id_hash = if device == "none" {
        None
    } else {
        Some(decode_hex_array::<32>(device, "storage ID SHA-256")?)
    };
    let partition_offset_bytes = parse_canonical_u64(
        exact_line(lines, &format!("{prefix}.Offset"))?,
        "extent offset",
    )?;
    let partition_length_bytes = parse_canonical_u64(
        exact_line(lines, &format!("{prefix}.Length"))?,
        "extent length",
    )?;
    let style = CanonicalTargetStyle::parse(exact_line(lines, &format!("{prefix}.Style"))?)?;
    let gpt = exact_line(lines, &format!("{prefix}.GptPartitionId"))?;
    let gpt_partition_id = if gpt == "none" {
        None
    } else {
        Some(decode_hex_array::<16>(gpt, "GPT partition identifier")?)
    };
    Ok(CanonicalInstallTargetV2 {
        layout_digest,
        device_id_hash,
        partition_offset_bytes,
        partition_length_bytes,
        style,
        gpt_partition_id,
    })
}

fn push_line(text: &mut String, name: &str, value: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(['\r', '\n', '='])
        || value.contains(['\r', '\n'])
        || value.is_empty()
    {
        bail!("handoff manifest field contains an invalid character");
    }
    text.push_str(name);
    text.push('=');
    text.push_str(value);
    text.push_str("\r\n");
    Ok(())
}

fn exact_line<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str) -> Result<&'a str> {
    lines
        .next()
        .and_then(|line| line.strip_prefix(&format!("{name}=")))
        .with_context(|| format!("handoff manifest is missing or reordered at {name}"))
}

fn parse_canonical_u64(value: &str, label: &str) -> Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("{label} is not a canonical unsigned integer");
    }
    value
        .parse()
        .with_context(|| format!("{label} is outside u64"))
}

fn artifact_key(value: &ArtifactRecord) -> (ArtifactRole, u32, ArtifactLocation, String) {
    (
        value.role,
        value.ordinal,
        value.location,
        value.relative_path.clone(),
    )
}

#[cfg(windows)]
fn paths_equal_ignore_case(left: &str, right: &str) -> Result<bool> {
    use windows::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
    let left = left.encode_utf16().collect::<Vec<_>>();
    let right = right.encode_utf16().collect::<Vec<_>>();
    let result = unsafe { CompareStringOrdinal(&left, &right, true) };
    if result.0 == 0 {
        bail!("CompareStringOrdinal failed while validating artifact paths");
    }
    Ok(result == CSTR_EQUAL)
}

#[cfg(not(windows))]
fn paths_equal_ignore_case(left: &str, right: &str) -> Result<bool> {
    Ok(left.to_lowercase() == right.to_lowercase())
}

fn validate_relative_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > HANDOFF_ARTIFACT_MAX_PATH_BYTES
        || value.starts_with(['\\', '/'])
        || value.contains('/')
    {
        bail!("handoff artifact path is not a bounded Windows relative path");
    }
    for component in value.split('\\') {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.ends_with(['.', ' '])
        {
            bail!("handoff artifact path contains an unsafe component");
        }
        if component.chars().any(|character| {
            character <= '\u{1f}' || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        }) {
            bail!("handoff artifact path contains an invalid Windows character");
        }
        let stem = component.split('.').next().unwrap_or(component);
        let upper = stem.to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || is_reserved_numbered_device(&upper)
        {
            bail!("handoff artifact path contains a reserved DOS device name");
        }
    }
    Ok(())
}

fn is_reserved_numbered_device(value: &str) -> bool {
    let suffix = value
        .strip_prefix("COM")
        .or_else(|| value.strip_prefix("LPT"));
    matches!(
        suffix,
        Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
    )
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => bail!("hexadecimal value is not canonical lowercase"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA_TOKEN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const TARGET_TOKEN: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const EMPTY_SHA256_HEX: &str =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn empty_sha256() -> [u8; 32] {
        assert_eq!(crate::hash::sha256_bytes(&[]), EMPTY_SHA256_HEX);
        crate::install_handoff::decode_hex_array::<32>(EMPTY_SHA256_HEX, "empty SHA-256").unwrap()
    }

    fn target(offset: u64, length: u64) -> CanonicalInstallTargetV2 {
        CanonicalInstallTargetV2 {
            layout_digest: [7; 32],
            device_id_hash: Some([8; 32]),
            partition_offset_bytes: offset,
            partition_length_bytes: length,
            style: CanonicalTargetStyle::Gpt,
            gpt_partition_id: Some([9; 16]),
        }
    }

    fn artifact(path: &str, ordinal: u32) -> ArtifactRecord {
        ArtifactRecord {
            role: ArtifactRole::InstallImageSpan,
            location: ArtifactLocation::PublicData,
            ordinal,
            relative_path: path.to_owned(),
            length_bytes: 123,
            sha256: [3; 32],
        }
    }

    #[test]
    fn canonical_roundtrip_binds_staging_and_artifacts() {
        let manifest = HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(TARGET_TOKEN.to_owned()),
            Some(AutoStagingAuthorization {
                source: target(1_048_576, 100_000),
                temporary: target(1_150_123, 200_000),
                source_length_before_bytes: 301_547,
            }),
            vec![
                artifact("images\\install.swm", 0),
                artifact("images\\install2.swm", 1),
                ArtifactRecord {
                    role: ArtifactRole::AutoPartitionMarker,
                    location: ArtifactLocation::PublicData,
                    ordinal: 0,
                    relative_path: "markers\\auto-created.txt".to_owned(),
                    length_bytes: 64,
                    sha256: [0x55; 32],
                },
            ],
        )
        .unwrap();
        let bytes = manifest.to_bytes().unwrap();
        assert_eq!(HandoffManifest::parse(&bytes).unwrap(), manifest);
        let binding = ManifestBinding::new(&bytes).unwrap();
        assert_eq!(binding.verify(&bytes).unwrap(), manifest);
        assert_eq!(
            binding
                .verify_for(
                    &bytes,
                    HandoffPurpose::Install,
                    "00112233445566778899aabbccddeeff"
                )
                .unwrap(),
            manifest
        );
        assert!(binding
            .verify_for(
                &bytes,
                HandoffPurpose::Backup,
                "00112233445566778899aabbccddeeff"
            )
            .is_err());
        let mut tampered = bytes;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(binding.verify(&tampered).is_err());
    }

    #[test]
    fn install_requires_two_independent_locator_tokens() {
        assert!(HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(DATA_TOKEN.to_owned()),
            None,
            vec![artifact("images\\install.wim", 0)],
        )
        .is_err());
        assert!(HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            None,
            None,
            vec![artifact("images\\install.wim", 0)],
        )
        .is_err());
    }

    #[test]
    fn secrets_and_case_insensitive_duplicates_fail_closed() {
        let mut secret = artifact("private\\answer.xml", 0);
        secret.role = ArtifactRole::CustomUnattend;
        assert!(HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(TARGET_TOKEN.to_owned()),
            None,
            vec![secret],
        )
        .is_err());
        assert!(HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(TARGET_TOKEN.to_owned()),
            None,
            vec![artifact("Images\\A.wim", 0), artifact("images\\a.WIM", 1),],
        )
        .is_err());
    }

    #[test]
    fn maintenance_accepts_only_one_protected_bitlocker_secret() {
        let secret = ArtifactRecord {
            role: ArtifactRole::ProtectedBitLockerSecret,
            location: ArtifactLocation::ProtectedBoot,
            ordinal: 0,
            relative_path: crate::bl_passthrough::KEYS_FILE_NAME.to_owned(),
            length_bytes: 64,
            sha256: [0x42; 32],
        };
        let maintenance = HandoffManifest::new(
            HandoffPurpose::Maintenance,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            None,
            None,
            vec![secret.clone()],
        )
        .unwrap();
        assert_eq!(maintenance.artifacts, vec![secret.clone()]);
        assert!(HandoffManifest::new(
            HandoffPurpose::Maintenance,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            None,
            None,
            vec![secret.clone(), secret.clone()],
        )
        .is_err());
        assert!(HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(TARGET_TOKEN.to_owned()),
            None,
            vec![secret],
        )
        .is_err());
    }

    #[test]
    fn image_spans_and_xp_tree_cannot_share_one_install_authorization() {
        let mut xp = artifact("xp\\I386\\setupldr.bin", 0);
        xp.role = ArtifactRole::XpSourceFile;
        assert!(HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(TARGET_TOKEN.to_owned()),
            None,
            vec![artifact("images\\install.wim", 0), xp],
        )
        .is_err());
    }

    #[test]
    fn unsafe_paths_and_overlapping_staging_are_rejected() {
        for path in [
            "..\\x.wim",
            "C:evil.wim",
            "dir\\CON.txt",
            "dir\\COM¹.txt",
            "dir\\tail. ",
        ] {
            assert!(artifact(path, 0).validate().is_err(), "{path}");
        }
        let with_provider_gap = AutoStagingAuthorization {
            source: target(10, 10),
            temporary: target(30, 10),
            source_length_before_bytes: 31,
        };
        assert_eq!(with_provider_gap.reclaim_length_bytes().unwrap(), 21);
        assert!(AutoStagingAuthorization {
            source: target(10, 10),
            temporary: target(30, 10),
            source_length_before_bytes: 29,
        }
        .validate()
        .is_err());
        assert!(AutoStagingAuthorization {
            source: target(10, 10),
            temporary: target(20, 10),
            source_length_before_bytes: 10,
        }
        .validate()
        .is_err());
        assert!(AutoStagingAuthorization {
            source: target(10, 20),
            temporary: target(20, 10),
            source_length_before_bytes: 30,
        }
        .validate()
        .is_err());
        let mut different_disk = target(20, 10);
        different_disk.device_id_hash = Some([0x44; 32]);
        assert!(AutoStagingAuthorization {
            source: target(10, 10),
            temporary: different_disk,
            source_length_before_bytes: 30,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn zero_length_ordinary_artifact_roles_roundtrip_with_real_empty_file_digest() {
        let empty_digest = empty_sha256();
        assert_ne!(empty_digest, [0; 32]);
        let artifacts = [
            (ArtifactRole::XpSourceFile, "xp\\I386\\empty.inf"),
            (
                ArtifactRole::PreservedDriver,
                "drivers\\preserved\\empty.cat",
            ),
            (ArtifactRole::UserDriver, "drivers\\user\\empty.dll"),
        ]
        .into_iter()
        .map(|(role, path)| {
            let mut empty = artifact(path, 0);
            empty.role = role;
            empty.length_bytes = 0;
            empty.sha256 = empty_digest;
            empty
        })
        .collect::<Vec<_>>();
        let manifest = HandoffManifest::new(
            HandoffPurpose::Install,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            Some(TARGET_TOKEN.to_owned()),
            None,
            artifacts,
        )
        .unwrap();
        let serialized = manifest.to_bytes().unwrap();
        let parsed = HandoffManifest::parse(&serialized).unwrap();
        assert_eq!(parsed, manifest);
        for role in [
            ArtifactRole::PreservedDriver,
            ArtifactRole::UserDriver,
            ArtifactRole::XpSourceFile,
        ] {
            let record = parsed
                .artifacts
                .iter()
                .find(|record| record.role == role)
                .unwrap();
            assert_eq!(record.length_bytes, 0);
            assert_eq!(record.sha256, empty_digest);
        }
    }

    #[test]
    fn zero_length_ordinary_artifact_roles_still_reject_all_zero_digest() {
        for (role, path) in [
            (
                ArtifactRole::PreservedDriver,
                "drivers\\preserved\\empty.inf",
            ),
            (ArtifactRole::UserDriver, "drivers\\user\\empty.inf"),
            (ArtifactRole::XpSourceFile, "xp\\I386\\empty.inf"),
        ] {
            let mut empty = artifact(path, 0);
            empty.role = role;
            empty.length_bytes = 0;
            empty.sha256 = [0; 32];
            let error = empty.validate().unwrap_err().to_string();
            assert!(
                error.contains("SHA-256 must not be all-zero"),
                "{role:?}: {error}"
            );
        }
    }

    #[test]
    fn zero_length_payload_object_roles_fail_before_consumer_write_boundaries() {
        for (role, path) in [
            (ArtifactRole::InstallImageSpan, "images\\install.wim"),
            (ArtifactRole::PcaPackage, "pca\\pca.wim"),
            (ArtifactRole::AutoPartitionMarker, "markers\\auto.marker"),
            (
                ArtifactRole::ProtectedAdministratorSecret,
                "secrets\\administrator.bin",
            ),
            (
                ArtifactRole::ProtectedBitLockerSecret,
                "secrets\\bitlocker.bin",
            ),
        ] {
            let mut empty = artifact(path, 0);
            empty.role = role;
            empty.length_bytes = 0;
            empty.sha256 = empty_sha256();
            if role.requires_protected_boot() {
                empty.location = ArtifactLocation::ProtectedBoot;
            }
            let error = empty.validate().unwrap_err().to_string();
            assert!(
                error.contains("requires a nonempty payload"),
                "{role:?}: {error}"
            );
        }
    }

    #[test]
    fn parser_rejects_reordered_or_noncanonical_bytes() {
        let mut base = artifact("images\\data.wim", 0);
        base.role = ArtifactRole::BackupBaseImage;
        let manifest = HandoffManifest::new(
            HandoffPurpose::Backup,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            None,
            None,
            vec![base],
        )
        .unwrap();
        let bytes = manifest.to_bytes().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(HandoffManifest::parse(
            text.replace("Artifact0000.Role", "Artifact0001.Role")
                .as_bytes()
        )
        .is_err());
        assert!(HandoffManifest::parse(text.replace("\r\n", "\n").as_bytes()).is_err());
    }

    #[test]
    fn config_binding_parser_is_unique_and_canonical() {
        let manifest = HandoffManifest::new(
            HandoffPurpose::Expand,
            "00112233445566778899aabbccddeeff",
            DATA_TOKEN,
            None,
            None,
            Vec::new(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let expected = ManifestBinding::new(&manifest).unwrap();
        let config = format!("[Expand]\r\n{}Target=C:\r\n", expected.to_config_lines());
        assert_eq!(
            ManifestBinding::from_config_bytes(config.as_bytes()).unwrap(),
            expected
        );
        assert!(ManifestBinding::from_config_bytes(
            format!("{config}HandoffManifestLength={}\r\n", manifest.len()).as_bytes()
        )
        .is_err());
        assert!(ManifestBinding::from_config_bytes(
            config
                .replace("HandoffManifestVersion=1", "HandoffManifestVersion=01")
                .as_bytes()
        )
        .is_err());
        assert!(ManifestBinding::from_config_bytes(
            config
                .replace("HandoffManifestSha256=", "HandoffManifestSha256=ABCDEF")
                .as_bytes()
        )
        .is_err());
    }
}
