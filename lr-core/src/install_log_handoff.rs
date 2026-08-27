//! Session-bound diagnostic log handoff for desktop -> WinPE -> installed Windows.
//!
//! Log persistence is diagnostic only: callers must turn failures into warnings
//! and must never weaken installation safety checks to preserve a log.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::operation::redact_log_text;
#[cfg(not(test))]
use crate::scoped_temp_file::restrict_to_system_and_administrators;
use crate::scoped_temp_file::{pin_existing_directory_ancestors, ScopedTempFile};

pub const LOG_HANDOFF_SCHEMA: u32 = 2;
const LEGACY_LOG_HANDOFF_SCHEMA: u32 = 1;
pub const MAX_STAGE_LOG_BYTES: u64 = 32 * 1024 * 1024;
pub const HANDOFF_LOG_DIRECTORY: &str = "logs";
pub const DESKTOP_LOG_FILE: &str = "normal.log";
pub const DESKTOP_MANIFEST_FILE: &str = "normal.manifest.json";
pub const PE_LOG_FILE: &str = "LetRecoveryPE.log";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const LOG_TAIL_TRUNCATION_MARKER: &[u8] = b"[TRUNCATED: retained complete-line tail]\r\n";

pub fn stable_log_destination_matches(
    expected: Option<crate::windows_storage::StableVolumeIdentity>,
    actual: Option<crate::windows_storage::StableVolumeIdentity>,
) -> bool {
    match (expected, actual) {
        (Some(expected), Some(actual)) => {
            crate::windows_storage::same_stable_volume_identity(expected, actual)
        }
        _ => false,
    }
}

fn restrict_log_acl(path: &Path) -> std::io::Result<()> {
    // Unit tests run without the elevated token required by the real install workflows. ACL
    // mutation itself is covered by the shared scoped-temp boundary; handoff tests exercise only
    // transaction, validation and merge semantics without modifying the developer machine ACL.
    #[cfg(test)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(test))]
    {
        restrict_to_system_and_administrators(path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopLogManifest {
    pub schema: u32,
    pub session_id: String,
    pub build: String,
    pub bytes: u64,
    pub sha256: String,
    /// Schema 2 stores the payload under an immutable content-addressed name. Publishing this
    /// manifest is the commit point, so an interrupted restage cannot invalidate an older pair.
    #[serde(default)]
    pub blob_file: String,
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid install-log session identifier");
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> Result<bool> {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect log handoff path: {}", path.display()))?;
    Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0)
}

#[cfg(not(windows))]
fn is_reparse_point(path: &Path) -> Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_type().is_symlink())
}

fn reject_existing_reparse_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(_) if is_reparse_point(ancestor)? => {
                bail!(
                    "install-log path contains a reparse point: {}",
                    ancestor.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn inspect_opened_regular_file(file: &fs::File) -> Result<fs::Metadata> {
    let metadata = file.metadata().context("inspect opened diagnostic file")?;
    if !metadata.is_file() {
        bail!("diagnostic source is not a regular file");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            bail!("diagnostic source is a reparse point");
        }
    }
    Ok(metadata)
}

fn read_strict_bounded_regular_file_from_handle(
    file: &fs::File,
    maximum_bytes: u64,
) -> Result<Vec<u8>> {
    let mut file = file.try_clone().context("clone opened diagnostic file")?;
    let metadata = inspect_opened_regular_file(&file)?;
    if metadata.len() > maximum_bytes {
        bail!("diagnostic source exceeds the handoff size limit");
    }
    file.seek(SeekFrom::Start(0))
        .context("rewind opened diagnostic file")?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut contents)
        .context("read opened diagnostic file")?;
    if contents.len() as u64 > maximum_bytes {
        bail!("diagnostic source grew beyond the handoff size limit while reading");
    }
    Ok(contents)
}

fn open_regular_file_without_following_final_reparse(path: &Path) -> Result<fs::File> {
    reject_existing_reparse_ancestors(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    options
        .open(path)
        .with_context(|| format!("open diagnostic file: {}", path.display()))
}

fn read_strict_bounded_regular_file(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>> {
    let parent = path.parent().context("diagnostic file has no parent")?;
    let _pins = pin_existing_directory_ancestors(parent)
        .with_context(|| format!("pin diagnostic path ancestors: {}", parent.display()))?;
    let file = open_regular_file_without_following_final_reparse(path)?;
    _pins.verify_unchanged().with_context(|| {
        format!(
            "verify diagnostic path ancestor identities: {}",
            parent.display()
        )
    })?;
    read_strict_bounded_regular_file_from_handle(&file, maximum_bytes)
        .with_context(|| format!("read diagnostic file: {}", path.display()))
}

fn read_complete_line_log_tail_from_handle(file: &fs::File, maximum_bytes: u64) -> Result<Vec<u8>> {
    let mut file = file.try_clone().context("clone opened diagnostic log")?;
    let metadata = inspect_opened_regular_file(&file)?;
    file.seek(SeekFrom::Start(0))
        .context("rewind opened diagnostic log")?;
    if metadata.len() <= maximum_bytes {
        let mut contents = Vec::with_capacity(metadata.len() as usize);
        Read::by_ref(&mut file)
            .take(maximum_bytes.saturating_add(1))
            .read_to_end(&mut contents)
            .context("read opened diagnostic log")?;
        if contents.len() as u64 <= maximum_bytes {
            return Ok(contents);
        }
    }

    if maximum_bytes <= LOG_TAIL_TRUNCATION_MARKER.len() as u64 {
        bail!("diagnostic log size limit is too small for the truncation marker");
    }
    let current_length = file
        .metadata()
        .context("refresh opened diagnostic log metadata")?
        .len();
    let content_budget = maximum_bytes - LOG_TAIL_TRUNCATION_MARKER.len() as u64;
    let start = current_length.saturating_sub(content_budget);
    file.seek(SeekFrom::Start(start))
        .context("seek to diagnostic log tail")?;
    let mut tail = Vec::with_capacity(content_budget as usize);
    file.take(content_budget.saturating_add(1))
        .read_to_end(&mut tail)
        .context("read diagnostic log tail")?;
    if tail.len() as u64 > content_budget {
        tail.remove(0);
    }
    if start > 0 {
        tail = match tail.iter().position(|byte| *byte == b'\n') {
            Some(newline) => tail.split_off(newline + 1),
            None => Vec::new(),
        };
    }
    let mut contents = Vec::with_capacity(LOG_TAIL_TRUNCATION_MARKER.len() + tail.len());
    contents.extend_from_slice(LOG_TAIL_TRUNCATION_MARKER);
    contents.extend_from_slice(&tail);
    Ok(contents)
}

fn read_complete_line_log_tail(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>> {
    let parent = path.parent().context("diagnostic log has no parent")?;
    let _pins = pin_existing_directory_ancestors(parent)
        .with_context(|| format!("pin diagnostic log ancestors: {}", parent.display()))?;
    let file = open_regular_file_without_following_final_reparse(path)?;
    _pins.verify_unchanged().with_context(|| {
        format!(
            "verify diagnostic log ancestor identities: {}",
            parent.display()
        )
    })?;
    read_complete_line_log_tail_from_handle(&file, maximum_bytes)
        .with_context(|| format!("read diagnostic log: {}", path.display()))
}

fn retain_complete_line_tail(mut contents: Vec<u8>, maximum_bytes: u64) -> Result<Vec<u8>> {
    if contents.len() as u64 <= maximum_bytes {
        return Ok(contents);
    }
    if maximum_bytes <= LOG_TAIL_TRUNCATION_MARKER.len() as u64 {
        bail!("diagnostic log size limit is too small for the truncation marker");
    }
    let content_budget = maximum_bytes as usize - LOG_TAIL_TRUNCATION_MARKER.len();
    let start = contents.len() - content_budget;
    let tail_start = contents[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|newline| start + newline + 1)
        .unwrap_or(contents.len());
    let tail = contents.split_off(tail_start);
    let mut bounded = Vec::with_capacity(LOG_TAIL_TRUNCATION_MARKER.len() + tail.len());
    bounded.extend_from_slice(LOG_TAIL_TRUNCATION_MARKER);
    bounded.extend_from_slice(&tail);
    Ok(bounded)
}

fn sanitize_and_bound_log(input: Vec<u8>) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(&input);
    retain_complete_line_tail(redact_log_text(&text).into_bytes(), MAX_STAGE_LOG_BYTES)
}

fn read_sanitized_log(path: &Path) -> Result<Vec<u8>> {
    let input = read_complete_line_log_tail(path, MAX_STAGE_LOG_BYTES)?;
    sanitize_and_bound_log(input)
}

fn read_sanitized_log_from_handle(file: &fs::File) -> Result<Vec<u8>> {
    let input = read_complete_line_log_tail_from_handle(file, MAX_STAGE_LOG_BYTES)?;
    sanitize_and_bound_log(input)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn atomic_publish(directory: &Path, name: &str, contents: &[u8]) -> Result<PathBuf> {
    atomic_publish_with_acl(directory, name, contents, true)
}

fn atomic_publish_diagnostic(directory: &Path, name: &str, contents: &[u8]) -> Result<PathBuf> {
    atomic_publish_with_acl(directory, name, contents, false)
}

fn enforce_or_warn_acl(path: &Path, required: bool, context: &str) -> Result<()> {
    if let Err(error) = restrict_log_acl(path) {
        if required {
            return Err(error).with_context(|| format!("{context}: {}", path.display()));
        }
        log::warn!(
            "[INSTALL LOG] diagnostic ACL hardening failed for {}; the sanitized log is still published: {error:#}",
            path.display()
        );
    }
    Ok(())
}

fn atomic_publish_with_acl(
    directory: &Path,
    name: &str,
    contents: &[u8],
    acl_required: bool,
) -> Result<PathBuf> {
    if !acl_required {
        return atomic_publish_best_effort_diagnostic(directory, name, contents);
    }
    // Check every existing ancestor before creating descendants so an already-present junction
    // cannot be traversed by `create_dir_all` before it is rejected.
    let _existing_pins = pin_existing_directory_ancestors(directory).with_context(|| {
        format!(
            "pin existing diagnostic directory ancestors: {}",
            directory.display()
        )
    })?;
    reject_existing_reparse_ancestors(directory)?;
    fs::create_dir_all(directory)
        .with_context(|| format!("create diagnostic directory: {}", directory.display()))?;
    let _complete_pins = pin_existing_directory_ancestors(directory).with_context(|| {
        format!(
            "pin complete diagnostic directory ancestors: {}",
            directory.display()
        )
    })?;
    reject_existing_reparse_ancestors(directory)?;
    if is_reparse_point(directory)? {
        bail!("diagnostic directory is a reparse point");
    }
    // A protected directory is required before any diagnostic bytes are created. Restricting only
    // the final file leaves a disclosure/replacement window on broadly writable data volumes.
    enforce_or_warn_acl(
        directory,
        acl_required,
        "restrict diagnostic directory ACL before publication",
    )?;
    _complete_pins.verify_unchanged().with_context(|| {
        format!(
            "verify diagnostic directory identities before publication: {}",
            directory.display()
        )
    })?;
    let (temporary, mut file) = ScopedTempFile::create_writer_in(directory, "lr-log", "tmp")
        .context("create diagnostic temporary file")?;
    file.write_all(contents)
        .context("write diagnostic temporary file")?;
    file.flush().context("flush diagnostic temporary file")?;
    file.sync_all().context("sync diagnostic temporary file")?;
    drop(file);
    enforce_or_warn_acl(
        temporary.path(),
        acl_required,
        "restrict diagnostic temporary file ACL",
    )?;
    _complete_pins.verify_unchanged().with_context(|| {
        format!(
            "verify diagnostic directory identities before commit: {}",
            directory.display()
        )
    })?;
    let target = directory.join(name);
    temporary
        .persist_replace(&target)
        .with_context(|| format!("publish diagnostic file: {}", target.display()))?;
    if read_strict_bounded_regular_file(&target, contents.len() as u64)
        .context("read back published diagnostic file")?
        != contents
    {
        bail!("published diagnostic file read-back differs");
    }
    enforce_or_warn_acl(
        &target,
        acl_required,
        "restrict published diagnostic file ACL",
    )?;
    Ok(target)
}

/// Compatibility-first publication for sanitized, non-authoritative diagnostics.
///
/// Unlike the authenticated handoff artifacts, a final install log is never consumed as an
/// authorization input. It still refuses an existing reparse point and uses an atomic temporary
/// file plus byte-for-byte readback, but it deliberately avoids holding and repeatedly comparing
/// every ancestor identity. Those extra inventory checks added race-prone failure points after the
/// system image had already been written and could cause the only useful failure log to disappear.
fn atomic_publish_best_effort_diagnostic(
    directory: &Path,
    name: &str,
    contents: &[u8],
) -> Result<PathBuf> {
    reject_existing_reparse_ancestors(directory)?;
    fs::create_dir_all(directory)
        .with_context(|| format!("create diagnostic directory: {}", directory.display()))?;
    reject_existing_reparse_ancestors(directory)?;
    if is_reparse_point(directory)? {
        bail!("diagnostic directory is a reparse point");
    }
    enforce_or_warn_acl(
        directory,
        false,
        "restrict diagnostic directory ACL before publication",
    )?;

    let (temporary, mut file) = ScopedTempFile::create_writer_in(directory, "lr-log", "tmp")
        .context("create diagnostic temporary file")?;
    file.write_all(contents)
        .context("write diagnostic temporary file")?;
    file.flush().context("flush diagnostic temporary file")?;
    file.sync_all().context("sync diagnostic temporary file")?;
    drop(file);
    enforce_or_warn_acl(
        temporary.path(),
        false,
        "restrict diagnostic temporary file ACL",
    )?;

    let target = directory.join(name);
    temporary
        .persist_replace(&target)
        .with_context(|| format!("publish diagnostic file: {}", target.display()))?;
    let opened = open_regular_file_without_following_final_reparse(&target)?;
    if read_strict_bounded_regular_file_from_handle(&opened, contents.len() as u64)
        .context("read back published diagnostic file")?
        != contents
    {
        bail!("published diagnostic file read-back differs");
    }
    enforce_or_warn_acl(&target, false, "restrict published diagnostic file ACL")?;
    Ok(target)
}

pub fn session_handoff_directory(data_directory: &Path, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(data_directory.join(HANDOFF_LOG_DIRECTORY).join(session_id))
}

/// Publish a sanitized desktop log and a hash-bound manifest into the PE data directory.
pub fn stage_desktop_log(
    source: &Path,
    data_directory: &Path,
    session_id: &str,
    build: &str,
) -> Result<DesktopLogManifest> {
    validate_session_id(session_id)?;
    let contents = read_sanitized_log(source)?;
    stage_desktop_log_contents(contents, data_directory, session_id, build)
}

/// Publish a desktop log from an already-open, identity-stable file object.
///
/// The caller is responsible for holding the original handle across its asynchronous writer
/// barrier. This function clones and rewinds that same file object and never reopens a pathname.
pub fn stage_desktop_log_from_file(
    source: &fs::File,
    data_directory: &Path,
    session_id: &str,
    build: &str,
) -> Result<DesktopLogManifest> {
    validate_session_id(session_id)?;
    let contents = read_sanitized_log_from_handle(source)?;
    stage_desktop_log_contents(contents, data_directory, session_id, build)
}

fn stage_desktop_log_contents(
    contents: Vec<u8>,
    data_directory: &Path,
    session_id: &str,
    build: &str,
) -> Result<DesktopLogManifest> {
    let directory = session_handoff_directory(data_directory, session_id)?;
    let sha256 = sha256_hex(&contents);
    let blob_file = format!("normal-{sha256}.log");
    atomic_publish(&directory, &blob_file, &contents)?;
    let manifest = DesktopLogManifest {
        schema: LOG_HANDOFF_SCHEMA,
        session_id: session_id.to_string(),
        build: build.to_string(),
        bytes: contents.len() as u64,
        sha256,
        blob_file,
    };
    let encoded = serde_json::to_vec_pretty(&manifest).context("encode desktop log manifest")?;
    atomic_publish(&directory, DESKTOP_MANIFEST_FILE, &encoded)?;
    Ok(manifest)
}

fn read_verified_staged_desktop_log(data_directory: &Path, session_id: &str) -> Result<Vec<u8>> {
    let directory = session_handoff_directory(data_directory, session_id)?;
    let manifest_path = directory.join(DESKTOP_MANIFEST_FILE);
    reject_existing_reparse_ancestors(&manifest_path)?;
    let manifest: DesktopLogManifest = serde_json::from_slice(&read_strict_bounded_regular_file(
        &manifest_path,
        MAX_MANIFEST_BYTES,
    )?)
    .context("parse desktop log manifest")?;
    if !matches!(
        manifest.schema,
        LOG_HANDOFF_SCHEMA | LEGACY_LOG_HANDOFF_SCHEMA
    ) || manifest.session_id != session_id
    {
        bail!("desktop log manifest does not match the active session");
    }
    if manifest.bytes > MAX_STAGE_LOG_BYTES
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("desktop log manifest contains invalid payload metadata");
    }
    let blob_file = if manifest.schema == LEGACY_LOG_HANDOFF_SCHEMA {
        if !manifest.blob_file.is_empty() {
            bail!("legacy desktop log manifest unexpectedly names a blob");
        }
        DESKTOP_LOG_FILE.to_string()
    } else {
        let expected = format!("normal-{}.log", manifest.sha256.to_ascii_lowercase());
        if manifest.blob_file != expected {
            bail!("desktop log manifest contains an invalid blob filename");
        }
        expected
    };
    let log_path = directory.join(blob_file);
    reject_existing_reparse_ancestors(&log_path)?;
    let contents = read_strict_bounded_regular_file(&log_path, MAX_STAGE_LOG_BYTES)?;
    if contents.len() as u64 != manifest.bytes || sha256_hex(&contents) != manifest.sha256 {
        bail!("staged desktop log size or SHA-256 does not match its manifest");
    }
    Ok(contents)
}

/// Copy the verified desktop log into the current WinPE program directory.
pub fn copy_desktop_log_to_pe(
    data_directory: &Path,
    pe_program_directory: &Path,
    session_id: &str,
) -> Result<PathBuf> {
    let contents = read_verified_staged_desktop_log(data_directory, session_id)?;
    atomic_publish(
        pe_program_directory,
        &format!("NormalEndpoint.{session_id}.log"),
        &contents,
    )
}

/// Merge the desktop and PE logs into one atomically published file on the new system.
pub fn publish_combined_install_log(
    desktop_log: Option<&Path>,
    pe_log: Option<&Path>,
    target_root: &Path,
    session_id: &str,
) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    let mut combined = Vec::new();
    combined.extend_from_slice(b"===== LetRecovery normal endpoint log =====\r\n");
    match desktop_log {
        Some(path) => match read_sanitized_log(path) {
            Ok(contents) => combined.extend_from_slice(&contents),
            Err(error) => {
                log::warn!(
                    "[INSTALL LOG] normal-endpoint log verification failed during final merge; publishing the PE log without it: {error:#}"
                );
                combined.extend_from_slice(b"[not available: verification failed]\r\n");
            }
        },
        None => combined.extend_from_slice(b"[not available]\r\n"),
    }
    combined.extend_from_slice(b"\r\n===== LetRecovery WinPE log =====\r\n");
    match pe_log {
        Some(path) => match read_sanitized_log(path) {
            Ok(contents) => combined.extend_from_slice(&contents),
            Err(error) => {
                log::warn!(
                    "[INSTALL LOG] WinPE log verification failed during final merge: {error:#}"
                );
                combined.extend_from_slice(b"[not available: verification failed]\r\n");
            }
        },
        None => combined.extend_from_slice(b"[not available]\r\n"),
    }
    let directory = target_root.join("LetRecovery").join("Logs");
    let latest = atomic_publish_diagnostic(&directory, "LetRecovery-install.log", &combined)?;
    if let Err(error) = atomic_publish_diagnostic(
        &directory,
        &format!("LetRecovery-install-{session_id}.log"),
        &combined,
    ) {
        log::warn!(
            "[INSTALL LOG] session-named diagnostic copy failed after the stable log was published: {error:#}"
        );
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temp_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lr-install-log-{name}-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn stable_log_volume(disk_number: u32) -> crate::windows_storage::StableVolumeIdentity {
        crate::windows_storage::StableVolumeIdentity {
            extent: crate::windows_storage::VolumeIdentity {
                disk_number,
                offset_bytes: 1_048_576,
                extent_length_bytes: 8 * 1024 * 1024,
            },
            disk: crate::windows_storage::StableDiskIdentity::Gpt { disk_id: [7; 16] },
            partition: crate::windows_storage::StablePartitionIdentity::Gpt {
                partition_id: [9; 16],
            },
            device_id_hash: Some([3; 32]),
        }
    }

    #[test]
    fn failure_log_destination_requires_the_same_stable_volume() {
        let expected = stable_log_volume(2);
        assert!(stable_log_destination_matches(
            Some(expected),
            Some(expected)
        ));
        assert!(!stable_log_destination_matches(
            Some(expected),
            Some(stable_log_volume(3))
        ));
        assert!(!stable_log_destination_matches(Some(expected), None));
        assert!(!stable_log_destination_matches(None, Some(expected)));
    }

    #[test]
    fn staged_log_is_session_bound_hashed_and_redacted() {
        let root = temp_directory("stage");
        let source = root.join("source.log");
        let data = root.join("data");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &source,
            "before password=hunter2 token=abc 111111-222222-333333-444444-555555-666666-777777-888888 after",
        )
        .unwrap();
        let manifest = stage_desktop_log(&source, &data, "session-1", "test").unwrap();
        assert_eq!(manifest.session_id, "session-1");
        let copied = copy_desktop_log_to_pe(&data, &root.join("pe"), "session-1").unwrap();
        let text = fs::read_to_string(copied).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("111111-222222"));
        assert!(text.contains("[REDACTED]"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_mismatch_is_rejected() {
        let root = temp_directory("tamper");
        let source = root.join("source.log");
        let data = root.join("data");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, "safe").unwrap();
        stage_desktop_log(&source, &data, "session-2", "test").unwrap();
        let manifest: DesktopLogManifest = serde_json::from_slice(
            &fs::read(
                session_handoff_directory(&data, "session-2")
                    .unwrap()
                    .join(DESKTOP_MANIFEST_FILE),
            )
            .unwrap(),
        )
        .unwrap();
        fs::write(
            session_handoff_directory(&data, "session-2")
                .unwrap()
                .join(manifest.blob_file),
            "tampered",
        )
        .unwrap();
        assert!(copy_desktop_log_to_pe(&data, &root.join("pe"), "session-2").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_open_handle_is_not_reopened_after_path_replacement() {
        let root = temp_directory("held-handle");
        let source = root.join("source.log");
        let moved = root.join("source.original.log");
        let data = root.join("data");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, "original password=first-secret\n").unwrap();
        let held = fs::File::open(&source).unwrap();

        fs::rename(&source, &moved).unwrap();
        fs::write(&source, "replacement password=second-secret\n").unwrap();

        stage_desktop_log_from_file(&held, &data, "session-held", "test").unwrap();
        let copied = copy_desktop_log_to_pe(&data, &root.join("pe"), "session-held").unwrap();
        let text = fs::read_to_string(copied).unwrap();
        assert!(text.contains("original"));
        assert!(!text.contains("replacement"));
        assert!(!text.contains("first-secret"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn merge_publishes_one_redacted_install_log() {
        let root = temp_directory("merge");
        let normal = root.join("normal.log");
        let pe = root.join("pe.log");
        fs::create_dir_all(&root).unwrap();
        fs::write(&normal, "normal token=desktop-secret\n").unwrap();
        fs::write(&pe, "pe password=pe-secret\n").unwrap();
        let output = publish_combined_install_log(
            Some(&normal),
            Some(&pe),
            &root.join("target"),
            "session-3",
        )
        .unwrap();
        assert_eq!(
            output.file_name().and_then(|name| name.to_str()),
            Some("LetRecovery-install.log")
        );
        assert!(output
            .parent()
            .unwrap()
            .join("LetRecovery-install-session-3.log")
            .is_file());
        let text = fs::read_to_string(output).unwrap();
        assert!(text.contains("normal endpoint"));
        assert!(text.contains("WinPE"));
        assert!(!text.contains("desktop-secret"));
        assert!(!text.contains("pe-secret"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_log_keeps_only_complete_line_tail_with_marker() {
        let root = temp_directory("bounded-tail");
        let source = root.join("source.log");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &source,
            b"discard-this-partial-line-abcdefghijklmnopqrstuvwxyz\r\nkeep-one\r\nkeep-two\r\n",
        )
        .unwrap();

        let contents = read_complete_line_log_tail(&source, 64).unwrap();
        let text = String::from_utf8(contents.clone()).unwrap();
        assert!(contents.len() <= 64);
        assert!(text.starts_with("[TRUNCATED: retained complete-line tail]\r\n"));
        assert!(!text.contains("discard-this-partial-line"));
        assert!(text.ends_with("keep-two\r\n"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_optional_desktop_log_does_not_hide_valid_pe_log() {
        let root = temp_directory("optional-desktop-failure");
        let missing = root.join("missing-normal.log");
        let pe = root.join("pe.log");
        fs::create_dir_all(&root).unwrap();
        fs::write(&pe, "PE terminal outcome is available\r\n").unwrap();

        let output = publish_combined_install_log(
            Some(&missing),
            Some(&pe),
            &root.join("target"),
            "session-optional",
        )
        .unwrap();
        let text = fs::read_to_string(output).unwrap();
        assert!(text.contains("[not available: verification failed]"));
        assert!(text.contains("PE terminal outcome is available"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_pe_log_does_not_hide_valid_desktop_log() {
        let root = temp_directory("optional-pe-missing");
        let normal = root.join("normal.log");
        fs::create_dir_all(&root).unwrap();
        fs::write(&normal, "normal endpoint reached reboot handoff\r\n").unwrap();

        let output = publish_combined_install_log(
            Some(&normal),
            None,
            &root.join("target"),
            "session-normal-only",
        )
        .unwrap();
        let text = fs::read_to_string(output).unwrap();
        assert!(text.contains("normal endpoint reached reboot handoff"));
        assert!(text.contains("LetRecovery WinPE log"));
        assert!(text.contains("[not available]"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_commit_switches_between_immutable_log_blobs() {
        let root = temp_directory("blob-commit");
        let source = root.join("source.log");
        let data = root.join("data");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, "first generation\r\n").unwrap();
        let first = stage_desktop_log(&source, &data, "session-blob", "test").unwrap();

        fs::write(&source, "second generation\r\n").unwrap();
        let second = stage_desktop_log(&source, &data, "session-blob", "test").unwrap();
        assert_ne!(first.blob_file, second.blob_file);
        let directory = session_handoff_directory(&data, "session-blob").unwrap();
        assert_eq!(
            fs::read_to_string(directory.join(&first.blob_file)).unwrap(),
            "first generation\r\n"
        );
        assert_eq!(
            fs::read_to_string(directory.join(&second.blob_file)).unwrap(),
            "second generation\r\n"
        );
        let copied = copy_desktop_log_to_pe(&data, &root.join("pe"), "session-blob").unwrap();
        assert_eq!(fs::read_to_string(copied).unwrap(), "second generation\r\n");
        fs::remove_dir_all(root).unwrap();
    }
}
