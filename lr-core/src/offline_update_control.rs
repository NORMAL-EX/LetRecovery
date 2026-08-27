//! Reversible, non-destructive Windows Update control for an offline Windows image.
//!
//! This module never deletes services, tasks or binaries and never changes service ACLs.
//! It captures a durable, strictly validated restore baseline before changing the documented
//! `NoAutoUpdate` policy. It never disables Windows Update, Orchestrator, Medic, Delivery
//! Optimization, BITS, Store, or manual update capability. The policy is intentionally
//! best-effort on consumer/Home editions
//! because Microsoft does not provide a permanent-disable contract there.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::registry::OfflineRegistry;
use crate::scoped_temp_file::{
    pin_existing_directory_ancestors, restrict_to_system_and_administrators, ScopedTempFile,
};

pub const UPDATE_CONTROL_SCHEMA: u32 = 6;
const LEGACY_OVERBROAD_UPDATE_CONTROL_SCHEMA: u32 = 5;
const LEGACY_UNBOUND_UPDATE_CONTROL_SCHEMA: u32 = 2;
const LEGACY_BOUND_UPDATE_CONTROL_SCHEMA: u32 = 3;
const LEGACY_OWNED_UPDATE_CONTROL_SCHEMA: u32 = 4;
pub const UPDATE_CONTROL_DIRECTORY: &str = "UpdateControl";
pub const UPDATE_CONTROL_MANIFEST: &str = "restore-v1.json";
pub const DISABLED_SERVICE_START: u32 = 4;
const INSTALLATION_BINDING_KEY: &str = "LetRecovery\\UpdateControl";
const INSTALLATION_BINDING_VALUE: &str = "InstallationId";

const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_CONTROL_SETS: usize = 16;
const LEGACY_UPDATE_SERVICES: [&str; 4] = ["wuauserv", "UsoSvc", "WaaSMedicSvc", "DoSvc"];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryScope {
    Software,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapturedDword {
    pub scope: RegistryScope,
    /// Key relative to the selected SOFTWARE or SYSTEM hive. A manifest never carries an
    /// executable HKLM path or a temporary hive alias.
    pub relative_key: String,
    pub value: String,
    pub previous: Option<u32>,
    pub applied: u32,
    /// True only after LetRecovery wrote this exact value, verified the registry readback and
    /// durably committed this ownership bit to the manifest. Restore must never infer ownership
    /// merely because the current registry value happens to equal `applied`.
    #[serde(default)]
    pub applied_by_letrecovery: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateControlManifest {
    pub schema: u32,
    /// Identifies the session which originally captured the baseline. A later session reuses a
    /// valid baseline so the original pre-LetRecovery values are not overwritten.
    pub session_id: String,
    /// Random identifier also written into this Windows installation's SOFTWARE hive. Applying a
    /// new image replaces the hive but can leave ProgramData behind when formatting is disabled;
    /// the second copy prevents a surviving manifest from being restored into that new image.
    #[serde(default)]
    pub installation_id: String,
    pub policy_values: Vec<CapturedDword>,
    pub service_values: Vec<CapturedDword>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpdateControlReport {
    pub applied_values: usize,
    pub already_applied_values: usize,
    pub baseline_reused: bool,
    pub missing_services: Vec<String>,
    pub warnings: Vec<String>,
}

fn validate_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid {label}");
    }
    Ok(())
}

fn effective_session_id(session_id: &str) -> Result<&str> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        // Legacy handoff files predate SessionId. Use a fixed, valid baseline owner rather than
        // silently turning the requested update control into a no-op.
        return Ok("legacy");
    }
    validate_component(session_id, "session identifier")?;
    Ok(session_id)
}

#[cfg(windows)]
fn new_installation_id() -> Result<String> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };

    let mut bytes = [0_u8; 16];
    let status = unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            &mut bytes,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status.0 != 0 {
        anyhow::bail!(
            "generate update-control installation identifier: BCryptGenRandom returned NTSTATUS 0x{:08X}",
            status.0 as u32
        );
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(not(windows))]
fn new_installation_id() -> Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    Ok(format!(
        "test{:016x}{:016x}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

fn manifest_path(target_root: &Path) -> PathBuf {
    target_root
        .join("ProgramData")
        .join("LetRecovery")
        .join(UPDATE_CONTROL_DIRECTORY)
        .join(UPDATE_CONTROL_MANIFEST)
}

#[cfg(windows)]
fn is_reparse(path: &Path) -> Result<bool> {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    Ok(fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0)
}

#[cfg(not(windows))]
fn is_reparse(path: &Path) -> Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_type().is_symlink())
}

fn reject_existing_reparse_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(_) if is_reparse(ancestor)? => {
                bail!(
                    "update-control path contains a reparse point: {}",
                    ancestor.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn secure_manifest_acl(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("update-control manifest has no parent")?;
    let _pins = pin_existing_directory_ancestors(parent).with_context(|| {
        format!(
            "pin update-control ACL path ancestors: {}",
            parent.display()
        )
    })?;
    restrict_to_system_and_administrators(parent).with_context(|| {
        format!(
            "restrict update-control directory ACL: {}",
            parent.display()
        )
    })?;
    restrict_to_system_and_administrators(path)
        .with_context(|| format!("restrict update-control manifest ACL: {}", path.display()))?;
    _pins.verify_unchanged().with_context(|| {
        format!(
            "verify update-control ACL path identities: {}",
            parent.display()
        )
    })?;
    Ok(())
}

fn read_manifest_bytes(path: &Path) -> Result<Vec<u8>> {
    let parent = path
        .parent()
        .context("update-control manifest has no parent")?;
    let _pins = pin_existing_directory_ancestors(parent).with_context(|| {
        format!(
            "pin update-control manifest ancestors: {}",
            parent.display()
        )
    })?;
    reject_existing_reparse_ancestors(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            bail!("update-control manifest is not a regular non-reparse file");
        }
        if metadata.len() > MAX_MANIFEST_BYTES {
            bail!("update-control manifest exceeds its size limit");
        }
        _pins.verify_unchanged().with_context(|| {
            format!(
                "verify update-control manifest ancestor identities: {}",
                parent.display()
            )
        })?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            bail!("update-control manifest grew beyond its size limit");
        }
        Ok(bytes)
    }
    #[cfg(not(windows))]
    {
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("update-control manifest is not a regular non-symlink file");
        }
        if metadata.len() > MAX_MANIFEST_BYTES {
            bail!("update-control manifest exceeds its size limit");
        }
        _pins.verify_unchanged().with_context(|| {
            format!(
                "verify update-control manifest ancestor identities: {}",
                parent.display()
            )
        })?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            bail!("update-control manifest grew beyond its size limit");
        }
        Ok(bytes)
    }
}

fn publish_manifest(path: &Path, manifest: &UpdateControlManifest) -> Result<()> {
    validate_manifest(manifest)?;
    if manifest.schema != UPDATE_CONTROL_SCHEMA {
        bail!("refusing to publish a legacy update-control manifest");
    }
    let parent = path
        .parent()
        .context("update-control manifest has no parent")?;
    let _existing_pins = pin_existing_directory_ancestors(parent).with_context(|| {
        format!(
            "pin existing update-control directory ancestors: {}",
            parent.display()
        )
    })?;
    reject_existing_reparse_ancestors(parent)?;
    fs::create_dir_all(parent).context("create update-control manifest directory")?;
    let _complete_pins = pin_existing_directory_ancestors(parent).with_context(|| {
        format!(
            "pin complete update-control directory ancestors: {}",
            parent.display()
        )
    })?;
    reject_existing_reparse_ancestors(parent)?;
    restrict_to_system_and_administrators(parent).with_context(|| {
        format!(
            "restrict update-control directory ACL: {}",
            parent.display()
        )
    })?;
    _complete_pins.verify_unchanged().with_context(|| {
        format!(
            "verify update-control directory identities before publication: {}",
            parent.display()
        )
    })?;
    let bytes = serde_json::to_vec_pretty(manifest).context("encode update-control manifest")?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("update-control manifest exceeds its size limit");
    }
    let (temporary, mut file) = ScopedTempFile::create_writer_in(parent, "update-control", "tmp")?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    _complete_pins.verify_unchanged().with_context(|| {
        format!(
            "verify update-control directory identities before manifest commit: {}",
            parent.display()
        )
    })?;
    temporary.persist_replace(path)?;
    secure_manifest_acl(path)?;
    if read_manifest_bytes(path)? != bytes {
        bail!("update-control manifest read-back differs");
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<UpdateControlManifest> {
    secure_manifest_acl(path)?;
    let manifest: UpdateControlManifest = serde_json::from_slice(&read_manifest_bytes(path)?)
        .context("parse update-control manifest")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn archive_stale_manifest(path: &Path, owner: &str) -> Result<PathBuf> {
    validate_component(owner, "stale manifest owner")?;
    let parent = path
        .parent()
        .context("update-control manifest has no parent")?;
    reject_existing_reparse_ancestors(parent)?;
    let _pins = pin_existing_directory_ancestors(parent).with_context(|| {
        format!(
            "pin stale update-control directory ancestors: {}",
            parent.display()
        )
    })?;
    let expected = read_manifest_bytes(path).context("read stale update-control manifest")?;
    _pins.verify_unchanged().with_context(|| {
        format!(
            "verify stale update-control directory identities: {}",
            parent.display()
        )
    })?;
    for suffix in 0..32_u32 {
        let archived = parent.join(format!("restore-stale-{owner}-{suffix}.json"));
        if archived.exists() {
            continue;
        }
        fs::rename(path, &archived).with_context(|| {
            format!(
                "archive stale update-control manifest: {} -> {}",
                path.display(),
                archived.display()
            )
        })?;
        secure_manifest_acl(&archived)?;
        if read_manifest_bytes(&archived)? != expected {
            bail!("archived update-control manifest read-back differs");
        }
        return Ok(archived);
    }
    bail!("too many archived update-control manifests")
}

fn parse_control_set_name(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("ControlSet")?;
    if digits.len() != 3 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse::<u32>().ok()?;
    (number != 0).then_some(number)
}

type PolicySpec = (&'static str, &'static str, u32);

const LEGACY_POLICY_ALLOWLIST: [PolicySpec; 3] = [
    (
        "Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
        "NoAutoUpdate",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows\\WindowsUpdate",
        "SetDisableUXWUAccess",
        1,
    ),
    (
        "Policies\\Microsoft\\Windows\\WindowsUpdate",
        "DoNotConnectToWindowsUpdateInternetLocations",
        1,
    ),
];

// Microsoft documents this value as the policy that disables Automatic Updates while retaining
// manual update capability. UX suppression, WSUS-only isolation and service disabling are
// deliberately outside this option's contract.
const POLICY_ALLOWLIST: [PolicySpec; 1] = [LEGACY_POLICY_ALLOWLIST[0]];

fn policy_allowlist_for_schema(schema: u32) -> &'static [PolicySpec] {
    if schema == UPDATE_CONTROL_SCHEMA {
        &POLICY_ALLOWLIST
    } else {
        match schema {
            LEGACY_UNBOUND_UPDATE_CONTROL_SCHEMA
            | LEGACY_BOUND_UPDATE_CONTROL_SCHEMA
            | LEGACY_OWNED_UPDATE_CONTROL_SCHEMA => &LEGACY_POLICY_ALLOWLIST[..2],
            LEGACY_OVERBROAD_UPDATE_CONTROL_SCHEMA => &LEGACY_POLICY_ALLOWLIST,
            _ => &[],
        }
    }
}

fn policy_allowlist() -> &'static [PolicySpec] {
    &POLICY_ALLOWLIST
}

fn capture_policy_value(
    software_hive: &str,
    (relative_key, value, applied): PolicySpec,
) -> Result<CapturedDword> {
    let key = format!("HKLM\\{software_hive}\\{relative_key}");
    Ok(CapturedDword {
        scope: RegistryScope::Software,
        relative_key: relative_key.to_string(),
        previous: OfflineRegistry::query_dword_optional(&key, value)?,
        value: value.to_string(),
        applied,
        applied_by_letrecovery: false,
    })
}

fn upgrade_policy_baseline(
    manifest: &mut UpdateControlManifest,
    software_hive: &str,
    system_hive: &str,
) -> Result<bool> {
    if manifest.schema == UPDATE_CONTROL_SCHEMA {
        return Ok(false);
    }
    // Schemas 2-5 included UX/WSUS policies and service Start mutations that do not mean
    // "disable automatic updates". Restore only values durably owned by LetRecovery and still
    // equal to the applied value, then drop those entries. A user/admin change is never undone.
    for value in manifest
        .policy_values
        .iter()
        .skip(1)
        .chain(manifest.service_values.iter())
    {
        if value.applied_by_letrecovery {
            let key = offline_key(value, software_hive, system_hive);
            if OfflineRegistry::query_dword_optional(&key, &value.value)? == Some(value.applied) {
                restore_registry_value(&key, &value.value, value.previous)?;
            }
        }
    }
    manifest.policy_values.retain(|value| {
        value.relative_key == POLICY_ALLOWLIST[0].0 && value.value == POLICY_ALLOWLIST[0].1
    });
    manifest.service_values.clear();
    for &spec in policy_allowlist() {
        if !manifest
            .policy_values
            .iter()
            .any(|value| value.relative_key == spec.0 && value.value == spec.1)
        {
            // Capture every newly allowlisted value before the first write. An older durable
            // ownership bit remains attached only to the exact legacy entry which earned it.
            manifest
                .policy_values
                .push(capture_policy_value(software_hive, spec)?);
        }
    }
    manifest.schema = UPDATE_CONTROL_SCHEMA;
    validate_manifest(manifest)?;
    Ok(true)
}

fn validate_policy_value(value: &CapturedDword) -> bool {
    value.scope == RegistryScope::Software
        && LEGACY_POLICY_ALLOWLIST.iter().any(|(key, name, applied)| {
            value.relative_key == *key && value.value == *name && value.applied == *applied
        })
}

fn validate_service_value(value: &CapturedDword) -> bool {
    if value.scope != RegistryScope::System
        || value.value != "Start"
        || value.applied != DISABLED_SERVICE_START
    {
        return false;
    }
    let mut components = value.relative_key.split('\\');
    let Some(control_set) = components.next() else {
        return false;
    };
    if parse_control_set_name(control_set).is_none() || components.next() != Some("Services") {
        return false;
    }
    let Some(service) = components.next() else {
        return false;
    };
    components.next().is_none()
        && LEGACY_UPDATE_SERVICES
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(service))
}

fn validate_manifest(manifest: &UpdateControlManifest) -> Result<()> {
    if !matches!(
        manifest.schema,
        UPDATE_CONTROL_SCHEMA
            | LEGACY_OWNED_UPDATE_CONTROL_SCHEMA
            | LEGACY_BOUND_UPDATE_CONTROL_SCHEMA
            | LEGACY_UNBOUND_UPDATE_CONTROL_SCHEMA
            | LEGACY_OVERBROAD_UPDATE_CONTROL_SCHEMA
    ) {
        bail!("unsupported update-control manifest schema");
    }
    validate_component(&manifest.session_id, "manifest session identifier")?;
    if matches!(
        manifest.schema,
        UPDATE_CONTROL_SCHEMA
            | LEGACY_OWNED_UPDATE_CONTROL_SCHEMA
            | LEGACY_BOUND_UPDATE_CONTROL_SCHEMA
    ) {
        validate_component(
            &manifest.installation_id,
            "manifest installation identifier",
        )?;
    } else if !manifest.installation_id.is_empty() {
        bail!("legacy update-control manifest unexpectedly contains an installation identifier");
    }
    if matches!(
        manifest.schema,
        LEGACY_BOUND_UPDATE_CONTROL_SCHEMA | LEGACY_UNBOUND_UPDATE_CONTROL_SCHEMA
    ) && manifest
        .policy_values
        .iter()
        .chain(manifest.service_values.iter())
        .any(|value| value.applied_by_letrecovery)
    {
        bail!("legacy update-control manifest unexpectedly claims write ownership");
    }
    let expected_policies = policy_allowlist_for_schema(manifest.schema);
    if manifest.policy_values.len() != expected_policies.len() {
        bail!("update-control manifest has an invalid policy entry count");
    }
    if manifest.schema == UPDATE_CONTROL_SCHEMA && !manifest.service_values.is_empty() {
        bail!("current update-control manifest must not contain service mutations");
    }
    if manifest.service_values.len() > MAX_CONTROL_SETS * LEGACY_UPDATE_SERVICES.len() {
        bail!("update-control manifest has too many service entries");
    }

    let mut identities = HashSet::new();
    for value in &manifest.policy_values {
        if !validate_policy_value(value) {
            bail!("update-control manifest contains a non-allowlisted policy entry");
        }
        let identity = (
            value.scope,
            value.relative_key.to_ascii_lowercase(),
            value.value.to_ascii_lowercase(),
        );
        if !identities.insert(identity) {
            bail!("update-control manifest contains a duplicate registry entry");
        }
    }
    for &(key, name, _) in expected_policies {
        if !manifest
            .policy_values
            .iter()
            .any(|value| value.relative_key == key && value.value == name)
        {
            bail!("update-control manifest is missing a required policy entry");
        }
    }
    for value in &manifest.service_values {
        if !validate_service_value(value) {
            bail!("update-control manifest contains a non-allowlisted service entry");
        }
        let identity = (
            value.scope,
            value.relative_key.to_ascii_lowercase(),
            value.value.to_ascii_lowercase(),
        );
        if !identities.insert(identity) {
            bail!("update-control manifest contains a duplicate registry entry");
        }
    }
    Ok(())
}

fn offline_key(value: &CapturedDword, software_hive: &str, system_hive: &str) -> String {
    let alias = match value.scope {
        RegistryScope::Software => software_hive,
        RegistryScope::System => system_hive,
    };
    format!("HKLM\\{alias}\\{}", value.relative_key)
}

fn online_key(value: &CapturedDword) -> String {
    let hive = match value.scope {
        RegistryScope::Software => "SOFTWARE",
        RegistryScope::System => "SYSTEM",
    };
    format!("HKLM\\{hive}\\{}", value.relative_key)
}

fn capture_manifest(
    session_id: &str,
    software_hive: &str,
    system_hive: &str,
) -> Result<UpdateControlManifest> {
    let session_id = effective_session_id(session_id)?;
    validate_component(software_hive, "SOFTWARE hive alias")?;
    validate_component(system_hive, "SYSTEM hive alias")?;

    let policy_values = policy_allowlist()
        .iter()
        .copied()
        .map(|spec| capture_policy_value(software_hive, spec))
        .collect::<Result<Vec<_>>>()?;

    let service_values = Vec::new();

    let manifest = UpdateControlManifest {
        schema: UPDATE_CONTROL_SCHEMA,
        session_id: session_id.to_string(),
        installation_id: new_installation_id()?,
        policy_values,
        service_values,
    };
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn installation_binding_key(software_hive: &str) -> String {
    format!("HKLM\\{software_hive}\\{INSTALLATION_BINDING_KEY}")
}

fn publish_installation_binding(software_hive: &str, installation_id: &str) -> Result<()> {
    validate_component(installation_id, "installation identifier")?;
    let key = installation_binding_key(software_hive);
    OfflineRegistry::set_string(&key, INSTALLATION_BINDING_VALUE, installation_id)?;
    if OfflineRegistry::query_string(&key, INSTALLATION_BINDING_VALUE)? != installation_id {
        bail!("update-control installation binding read-back differs");
    }
    Ok(())
}

fn verify_installation_binding_online(installation_id: &str) -> Result<()> {
    validate_component(installation_id, "installation identifier")?;
    let key = format!("HKLM\\SOFTWARE\\{INSTALLATION_BINDING_KEY}");
    let actual = OfflineRegistry::query_string(&key, INSTALLATION_BINDING_VALUE)
        .context("read current Windows update-control installation binding")?;
    if actual != installation_id {
        bail!("update-control manifest belongs to a different Windows installation");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyPlan {
    AlreadyApplied,
    Apply,
    SkipOwnershipLost,
}

fn plan_apply(
    current: Result<Option<u32>>,
    applied: u32,
    applied_by_letrecovery: bool,
) -> Result<ApplyPlan> {
    match current? {
        Some(current) if current == applied => Ok(ApplyPlan::AlreadyApplied),
        _ if applied_by_letrecovery => Ok(ApplyPlan::SkipOwnershipLost),
        _ => Ok(ApplyPlan::Apply),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyOutcome {
    AlreadyApplied,
    Applied { previous: Option<u32> },
    OwnershipLost,
    Failed,
}

fn apply_value(key: &str, value: &CapturedDword, report: &mut UpdateControlReport) -> ApplyOutcome {
    let previous = match OfflineRegistry::query_dword_optional(key, &value.value) {
        Ok(previous) => previous,
        Err(error) => {
            report.warnings.push(format!(
                "{key}\\{}: pre-write query failed; skipped: {error}",
                value.value
            ));
            return ApplyOutcome::Failed;
        }
    };
    match plan_apply(Ok(previous), value.applied, value.applied_by_letrecovery) {
        Ok(ApplyPlan::AlreadyApplied) => {
            report.already_applied_values += 1;
            return ApplyOutcome::AlreadyApplied;
        }
        Ok(ApplyPlan::SkipOwnershipLost) => {
            report.warnings.push(format!(
                "{key}\\{} changed after LetRecovery applied it; reapply skipped and durable ownership will be cleared",
                value.value
            ));
            return ApplyOutcome::OwnershipLost;
        }
        Ok(ApplyPlan::Apply) => {}
        Err(error) => {
            report.warnings.push(format!(
                "{key}\\{}: pre-write query failed; skipped: {error}",
                value.value
            ));
            return ApplyOutcome::Failed;
        }
    }
    let result = OfflineRegistry::set_dword(key, &value.value, value.applied).and_then(|()| {
        if OfflineRegistry::query_dword(key, &value.value)? != value.applied {
            bail!("registry read-back mismatch")
        }
        Ok(())
    });
    match result {
        Ok(()) => ApplyOutcome::Applied { previous },
        Err(error) => {
            report
                .warnings
                .push(format!("{key}\\{}: {error}", value.value));
            ApplyOutcome::Failed
        }
    }
}

fn clear_applied_ownership(
    manifest_path: &Path,
    manifest: &mut UpdateControlManifest,
    policy: bool,
    index: usize,
) -> Result<()> {
    let value = if policy {
        &mut manifest.policy_values[index]
    } else {
        &mut manifest.service_values[index]
    };
    if !value.applied_by_letrecovery {
        return Ok(());
    }
    value.applied_by_letrecovery = false;
    if let Err(error) = publish_manifest(manifest_path, manifest) {
        let value = if policy {
            &mut manifest.policy_values[index]
        } else {
            &mut manifest.service_values[index]
        };
        value.applied_by_letrecovery = true;
        return Err(error).context("persist lost update-control ownership");
    }
    Ok(())
}

fn restore_registry_value(key: &str, name: &str, previous: Option<u32>) -> Result<()> {
    match previous {
        Some(previous) => OfflineRegistry::set_dword(key, name, previous).and_then(|()| {
            if OfflineRegistry::query_dword_optional(key, name)? != Some(previous) {
                bail!("registry rollback read-back mismatch")
            }
            Ok(())
        }),
        None => OfflineRegistry::delete_value(key, name).and_then(|()| {
            if OfflineRegistry::query_dword_optional(key, name)?.is_some() {
                bail!("registry rollback deletion read-back mismatch")
            }
            Ok(())
        }),
    }
}

fn commit_applied_ownership(
    manifest_path: &Path,
    manifest: &mut UpdateControlManifest,
    policy: bool,
    index: usize,
    key: &str,
    previous: Option<u32>,
    report: &mut UpdateControlReport,
) {
    let value = if policy {
        &mut manifest.policy_values[index]
    } else {
        &mut manifest.service_values[index]
    };
    if value.applied_by_letrecovery {
        report.applied_values += 1;
        return;
    }
    value.applied_by_letrecovery = true;
    let name = value.value.clone();
    if let Err(error) = publish_manifest(manifest_path, manifest) {
        let value = if policy {
            &mut manifest.policy_values[index]
        } else {
            &mut manifest.service_values[index]
        };
        value.applied_by_letrecovery = false;
        match restore_registry_value(key, &name, previous) {
            Ok(()) => report.warnings.push(format!(
                "{key}\\{name}: ownership manifest commit failed; registry write was rolled back: {error}"
            )),
            Err(rollback) => report.warnings.push(format!(
                "{key}\\{name}: ownership manifest commit failed and registry rollback also failed; value remains unowned and online restore will refuse it: {error}; rollback: {rollback}"
            )),
        }
    } else {
        report.applied_values += 1;
    }
}

/// Capture the original state, then apply the reversible `NoAutoUpdate` policy.
///
/// A corrupt baseline causes a zero-write error. A valid baseline from an older session is reused
/// deliberately, preserving the real pre-LetRecovery values. Individual mutations are best-effort
/// and are returned as warnings so installation can continue without a dialog.
pub fn apply_offline_update_control(
    target_root: &Path,
    session_id: &str,
    software_hive: &str,
    system_hive: &str,
) -> Result<UpdateControlReport> {
    let session_id = effective_session_id(session_id)?;
    validate_component(software_hive, "SOFTWARE hive alias")?;
    validate_component(system_hive, "SYSTEM hive alias")?;
    let path = manifest_path(target_root);
    let (mut manifest, baseline_reused) = if path.exists() {
        let mut manifest = read_manifest(&path)?;
        if manifest.schema == LEGACY_UNBOUND_UPDATE_CONTROL_SCHEMA {
            if manifest.session_id != session_id {
                archive_stale_manifest(&path, &manifest.session_id)?;
                let manifest = capture_manifest(session_id, software_hive, system_hive)?;
                publish_manifest(&path, &manifest)?;
                publish_installation_binding(software_hive, &manifest.installation_id)?;
                (manifest, false)
            } else {
                manifest.installation_id = new_installation_id()?;
                upgrade_policy_baseline(&mut manifest, software_hive, system_hive)?;
                publish_manifest(&path, &manifest)?;
                publish_installation_binding(software_hive, &manifest.installation_id)?;
                (manifest, true)
            }
        } else {
            let key = installation_binding_key(software_hive);
            let actual = OfflineRegistry::query_string_optional(&key, INSTALLATION_BINDING_VALUE)
                .context("read offline update-control installation binding")?;
            if actual.as_deref() == Some(manifest.installation_id.as_str()) {
                let mut changed = false;
                if manifest.schema == LEGACY_BOUND_UPDATE_CONTROL_SCHEMA {
                    // Schema 3 had an installation binding but no durable per-value ownership.
                    // Upgrade conservatively: equal current values are not evidence that this
                    // process wrote them, so every entry remains unowned until a later real write.
                    for value in manifest
                        .policy_values
                        .iter_mut()
                        .chain(manifest.service_values.iter_mut())
                    {
                        value.applied_by_letrecovery = false;
                    }
                    changed = true;
                }
                if upgrade_policy_baseline(&mut manifest, software_hive, system_hive)? {
                    changed = true;
                }
                if changed {
                    publish_manifest(&path, &manifest)?;
                }
                (manifest, true)
            } else {
                archive_stale_manifest(&path, &manifest.installation_id)?;
                let manifest = capture_manifest(session_id, software_hive, system_hive)?;
                publish_manifest(&path, &manifest)?;
                publish_installation_binding(software_hive, &manifest.installation_id)?;
                (manifest, false)
            }
        }
    } else {
        let manifest = capture_manifest(session_id, software_hive, system_hive)?;
        publish_manifest(&path, &manifest)?;
        publish_installation_binding(software_hive, &manifest.installation_id)?;
        (manifest, false)
    };

    let mut report = UpdateControlReport {
        baseline_reused,
        ..UpdateControlReport::default()
    };
    for index in 0..manifest.policy_values.len() {
        let value = manifest.policy_values[index].clone();
        let key = offline_key(&value, software_hive, system_hive);
        match apply_value(&key, &value, &mut report) {
            ApplyOutcome::Applied { previous } => commit_applied_ownership(
                &path,
                &mut manifest,
                true,
                index,
                &key,
                previous,
                &mut report,
            ),
            ApplyOutcome::OwnershipLost => {
                clear_applied_ownership(&path, &mut manifest, true, index)?;
            }
            ApplyOutcome::AlreadyApplied | ApplyOutcome::Failed => {}
        }
    }
    for index in 0..manifest.service_values.len() {
        let value = manifest.service_values[index].clone();
        let key = offline_key(&value, software_hive, system_hive);
        match OfflineRegistry::key_exists(&key) {
            Ok(true) => {}
            Ok(false) => {
                report.warnings.push(format!(
                    "{key}: captured service key is absent in the current image; skipped"
                ));
                continue;
            }
            Err(error) => {
                report
                    .warnings
                    .push(format!("{key}: service existence check failed: {error}"));
                continue;
            }
        }
        match apply_value(&key, &value, &mut report) {
            ApplyOutcome::Applied { previous } => commit_applied_ownership(
                &path,
                &mut manifest,
                false,
                index,
                &key,
                previous,
                &mut report,
            ),
            ApplyOutcome::OwnershipLost => {
                clear_applied_ownership(&path, &mut manifest, false, index)?;
            }
            ApplyOutcome::AlreadyApplied | ApplyOutcome::Failed => {}
        }
    }
    debug_assert!(manifest.service_values.is_empty());
    Ok(report)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestorePlan {
    Restore,
    SkipUnowned,
    SkipChanged,
}

fn plan_restore(value: &CapturedDword, current: Option<u32>) -> RestorePlan {
    if !value.applied_by_letrecovery {
        RestorePlan::SkipUnowned
    } else if current != Some(value.applied) {
        RestorePlan::SkipChanged
    } else {
        RestorePlan::Restore
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestoreOutcome {
    Restored,
    OwnershipLost,
    Skipped,
}

fn restore_value_online(value: &CapturedDword, report: &mut UpdateControlReport) -> RestoreOutcome {
    let key = online_key(value);
    let current = match OfflineRegistry::query_dword_optional(&key, &value.value) {
        Ok(current) => current,
        Err(error) => {
            report
                .warnings
                .push(format!("{key}\\{}: {error}", value.value));
            return RestoreOutcome::Skipped;
        }
    };
    match plan_restore(value, current) {
        RestorePlan::Restore => {}
        RestorePlan::SkipUnowned => return RestoreOutcome::Skipped,
        RestorePlan::SkipChanged => {
            report.warnings.push(format!(
                "{key}\\{} changed after LetRecovery applied it; restore skipped and durable ownership will be cleared",
                value.value
            ));
            return RestoreOutcome::OwnershipLost;
        }
    }
    let result = match value.previous {
        Some(previous) => OfflineRegistry::set_dword(&key, &value.value, previous).and_then(|()| {
            if OfflineRegistry::query_dword_optional(&key, &value.value)? != Some(previous) {
                bail!("registry restore read-back mismatch")
            }
            Ok(())
        }),
        None => OfflineRegistry::delete_value(&key, &value.value).and_then(|()| {
            if OfflineRegistry::query_dword_optional(&key, &value.value)?.is_some() {
                bail!("registry deletion read-back mismatch")
            }
            Ok(())
        }),
    };
    match result {
        Ok(()) => {
            report.applied_values += 1;
            RestoreOutcome::Restored
        }
        Err(error) => {
            report
                .warnings
                .push(format!("{key}\\{}: {error}", value.value));
            RestoreOutcome::Skipped
        }
    }
}

/// Restore the captured values in the running installed Windows instance.
///
/// The manifest path selects only the baseline file. Registry destinations are reconstructed from
/// the validated scope and fixed allowlist, and always resolve to online HKLM\\SOFTWARE or
/// HKLM\\SYSTEM. Values are restored only while they still equal LetRecovery's applied state.
pub fn restore_online_update_control(target_root: &Path) -> Result<UpdateControlReport> {
    let path = manifest_path(target_root);
    let mut manifest = read_manifest(&path)?;
    if !matches!(
        manifest.schema,
        UPDATE_CONTROL_SCHEMA
            | LEGACY_OVERBROAD_UPDATE_CONTROL_SCHEMA
            | LEGACY_OWNED_UPDATE_CONTROL_SCHEMA
    ) {
        bail!("legacy update-control baseline is not bound to this Windows installation");
    }
    verify_installation_binding_online(&manifest.installation_id)?;
    if upgrade_policy_baseline(&mut manifest, "SOFTWARE", "SYSTEM")? {
        publish_manifest(&path, &manifest)?;
    }
    let mut report = UpdateControlReport {
        baseline_reused: true,
        ..UpdateControlReport::default()
    };
    for index in 0..manifest.service_values.len() {
        let value = manifest.service_values[index].clone();
        let outcome = restore_value_online(&value, &mut report);
        match outcome {
            RestoreOutcome::Restored | RestoreOutcome::OwnershipLost => {
                if let Err(error) = clear_applied_ownership(&path, &mut manifest, false, index) {
                    let action = if outcome == RestoreOutcome::Restored {
                        "restored"
                    } else {
                        "found changed"
                    };
                    report.warnings.push(format!(
                        "{}\\{}: {action}, but clearing durable ownership failed: {error}",
                        online_key(&value),
                        value.value
                    ));
                }
            }
            RestoreOutcome::Skipped => {}
        }
    }
    for index in 0..manifest.policy_values.len() {
        let value = manifest.policy_values[index].clone();
        match restore_value_online(&value, &mut report) {
            RestoreOutcome::Restored | RestoreOutcome::OwnershipLost => {
                if let Err(error) = clear_applied_ownership(&path, &mut manifest, true, index) {
                    report.warnings.push(format!(
                        "{}\\{}: restored or found changed, but clearing durable ownership failed: {error}",
                        online_key(&value),
                        value.value
                    ));
                }
            }
            RestoreOutcome::Skipped => {}
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> UpdateControlManifest {
        UpdateControlManifest {
            schema: UPDATE_CONTROL_SCHEMA,
            session_id: "session-1".to_string(),
            installation_id: "installation-1".to_string(),
            policy_values: policy_allowlist()
                .iter()
                .copied()
                .map(|(key, value, applied)| CapturedDword {
                    scope: RegistryScope::Software,
                    relative_key: key.to_string(),
                    value: value.to_string(),
                    previous: None,
                    applied,
                    applied_by_letrecovery: false,
                })
                .collect(),
            service_values: Vec::new(),
        }
    }

    fn use_legacy_policy_shape(manifest: &mut UpdateControlManifest) {
        if !manifest
            .policy_values
            .iter()
            .any(|value| value.value == "SetDisableUXWUAccess")
        {
            manifest.policy_values.push(CapturedDword {
                scope: RegistryScope::Software,
                relative_key: LEGACY_POLICY_ALLOWLIST[1].0.to_string(),
                value: LEGACY_POLICY_ALLOWLIST[1].1.to_string(),
                previous: None,
                applied: 1,
                applied_by_letrecovery: false,
            });
        }
    }

    #[test]
    fn automatic_update_scope_is_only_the_documented_policy() {
        assert_eq!(policy_allowlist(), &[POLICY_ALLOWLIST[0]]);
        assert_eq!(POLICY_ALLOWLIST[0].1, "NoAutoUpdate");
        assert!(LEGACY_UPDATE_SERVICES.contains(&"wuauserv"));
        assert!(!policy_allowlist().iter().any(|(key, value, _)| {
            key.contains("Windows Defender")
                || value.eq_ignore_ascii_case("DisableAntiSpyware")
                || value.eq_ignore_ascii_case("DisableAntiVirus")
                || value.eq_ignore_ascii_case("SetDisableUXWUAccess")
                || value.eq_ignore_ascii_case("DoNotConnectToWindowsUpdateInternetLocations")
        }));
    }

    #[test]
    fn manifest_roundtrip_uses_scopes_and_relative_paths() {
        let manifest = valid_manifest();
        validate_manifest(&manifest).unwrap();
        let encoded = serde_json::to_vec(&manifest).unwrap();
        let decoded = serde_json::from_slice::<UpdateControlManifest>(&encoded).unwrap();
        assert_eq!(decoded, manifest);
        assert!(!String::from_utf8(encoded).unwrap().contains("HKLM"));
    }

    #[test]
    fn validator_rejects_arbitrary_registry_destination() {
        let mut manifest = valid_manifest();
        manifest.service_values.push(CapturedDword {
            scope: RegistryScope::System,
            relative_key: "ControlSet001\\Services\\UnrelatedService".to_string(),
            value: "Start".to_string(),
            previous: Some(2),
            applied: DISABLED_SERVICE_START,
            applied_by_letrecovery: false,
        });
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn validator_rejects_duplicate_and_wrong_applied_values() {
        let mut duplicate = valid_manifest();
        duplicate
            .policy_values
            .push(duplicate.policy_values[0].clone());
        assert!(validate_manifest(&duplicate).is_err());

        let mut wrong_applied = valid_manifest();
        wrong_applied.policy_values[0].applied = 3;
        assert!(validate_manifest(&wrong_applied).is_err());
    }

    #[test]
    fn schema_three_requires_an_installation_binding_and_legacy_cannot_forge_one() {
        let mut current = valid_manifest();
        current.installation_id.clear();
        assert!(validate_manifest(&current).is_err());

        let mut legacy = valid_manifest();
        legacy.schema = LEGACY_UNBOUND_UPDATE_CONTROL_SCHEMA;
        use_legacy_policy_shape(&mut legacy);
        assert!(validate_manifest(&legacy).is_err());
        legacy.installation_id.clear();
        assert!(validate_manifest(&legacy).is_ok());
        legacy.policy_values[0].applied_by_letrecovery = true;
        assert!(validate_manifest(&legacy).is_err());
    }

    #[test]
    fn online_and_offline_paths_are_reconstructed_not_loaded_from_json() {
        let policy = &valid_manifest().policy_values[0];
        assert_eq!(
            offline_key(policy, "pc-soft", "pc-sys"),
            "HKLM\\pc-soft\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU"
        );
        assert_eq!(
            online_key(policy),
            "HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU"
        );
    }

    #[test]
    fn control_set_parser_is_exact() {
        assert_eq!(parse_control_set_name("ControlSet001"), Some(1));
        assert_eq!(parse_control_set_name("ControlSet999"), Some(999));
        assert_eq!(parse_control_set_name("ControlSet000"), None);
        assert_eq!(parse_control_set_name("ControlSet01"), None);
        assert_eq!(parse_control_set_name("Other001"), None);
    }

    #[test]
    fn legacy_empty_session_gets_a_stable_baseline_owner() {
        assert_eq!(effective_session_id("").unwrap(), "legacy");
        assert_eq!(effective_session_id("  ").unwrap(), "legacy");
        assert_eq!(effective_session_id("session-2").unwrap(), "session-2");
        assert!(effective_session_id("bad/session").is_err());
    }

    #[test]
    fn apply_planning_never_converts_a_query_error_into_permission_to_write() {
        assert_eq!(
            plan_apply(
                Ok(Some(DISABLED_SERVICE_START)),
                DISABLED_SERVICE_START,
                false,
            )
            .unwrap(),
            ApplyPlan::AlreadyApplied
        );
        assert_eq!(
            plan_apply(Ok(Some(2)), DISABLED_SERVICE_START, false).unwrap(),
            ApplyPlan::Apply
        );
        assert_eq!(
            plan_apply(Ok(None), DISABLED_SERVICE_START, false).unwrap(),
            ApplyPlan::Apply
        );
        assert!(plan_apply(
            Err(anyhow::anyhow!("query failed")),
            DISABLED_SERVICE_START,
            false,
        )
        .is_err());
        assert_eq!(
            plan_apply(Ok(Some(2)), DISABLED_SERVICE_START, true).unwrap(),
            ApplyPlan::SkipOwnershipLost
        );
    }

    #[test]
    fn restore_requires_durable_ownership_in_addition_to_value_equality() {
        let mut value = valid_manifest().policy_values.remove(0);
        assert_eq!(
            plan_restore(&value, Some(value.applied)),
            RestorePlan::SkipUnowned
        );
        value.applied_by_letrecovery = true;
        assert_eq!(plan_restore(&value, Some(2)), RestorePlan::SkipChanged);
        assert_eq!(
            plan_restore(&value, Some(value.applied)),
            RestorePlan::Restore
        );
    }

    #[test]
    fn schema_three_deserializes_as_unowned_and_cannot_be_restored_by_inference() {
        let mut manifest = valid_manifest();
        manifest.schema = LEGACY_BOUND_UPDATE_CONTROL_SCHEMA;
        use_legacy_policy_shape(&mut manifest);
        let mut json = serde_json::to_value(&manifest).unwrap();
        for value in json["policy_values"].as_array_mut().unwrap() {
            value
                .as_object_mut()
                .unwrap()
                .remove("applied_by_letrecovery");
        }
        for value in json["service_values"].as_array_mut().unwrap() {
            value
                .as_object_mut()
                .unwrap()
                .remove("applied_by_letrecovery");
        }
        let decoded: UpdateControlManifest = serde_json::from_value(json).unwrap();
        assert!(decoded
            .policy_values
            .iter()
            .chain(decoded.service_values.iter())
            .all(|value| !value.applied_by_letrecovery));
    }

    #[test]
    fn schema_four_owned_baseline_remains_restore_compatible() {
        let mut manifest = valid_manifest();
        manifest.schema = LEGACY_OWNED_UPDATE_CONTROL_SCHEMA;
        use_legacy_policy_shape(&mut manifest);
        manifest.policy_values[0].applied_by_letrecovery = true;
        validate_manifest(&manifest).expect("schema-four ownership must remain readable");
        assert_eq!(
            plan_restore(&manifest.policy_values[0], Some(1)),
            RestorePlan::Restore
        );
    }
}
