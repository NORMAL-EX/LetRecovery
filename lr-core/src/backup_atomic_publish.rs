//! Crash-recoverable, compare-and-swap publication of completed backup images.
//!
//! This module deliberately does not capture or modify WIM/ESD contents. A caller first creates a
//! complete staged image in the session directory, closes its writer, and supplies the expected
//! byte length and SHA-256. Publication then keeps DELETE-capable handles open while refusing
//! concurrent write sharing. The handles share delete because supported WinPE rename paths may
//! internally reopen the same object; exact file IDs, no-replace renames and post-readback retain
//! the CAS boundary. A checksummed PREPARED journal is persisted before the first rename. Recovery
//! classifies the actual old/new objects by both their hashes and file IDs; journal phase is never
//! treated as truth.

use crate::handoff_auth::SessionId;
use crate::scoped_temp_file::{
    create_system_administrators_file_new, pin_existing_parent_directory_ancestors,
    verify_system_administrators_directory_custody, verify_system_administrators_file_custody,
    PinnedDirectoryAncestors, ScopedTempDir,
};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const JOURNAL_MAGIC: &str = "LRBPC1";
const JOURNAL_MAX_BYTES: usize = 4096;
const HASH_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistingPublishKind {
    Replace,
    Append,
}

impl ExistingPublishKind {
    fn mode(self) -> PublishMode {
        match self {
            Self::Replace => PublishMode::Replace,
            Self::Append => PublishMode::Append,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishMode {
    Create,
    Replace,
    Append,
}

impl PublishMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Replace => "replace",
            Self::Append => "append",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "create" => Ok(Self::Create),
            "replace" => Ok(Self::Replace),
            "append" => Ok(Self::Append),
            _ => bail!("backup publish journal contains an unknown mode"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileExpectation {
    pub length: u64,
    pub sha256: [u8; 32],
}

impl FileExpectation {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            length: bytes.len() as u64,
            sha256: Sha256::digest(bytes).into(),
        }
    }
}

/// A same-parent session directory created with a protected SYSTEM/Administrators descriptor.
///
/// The target/session file IDs are captured from exact verified handles, then those two handles
/// are closed.  Some supported WinPE/NTFS combinations reject child hard-link and rename requests
/// while *any* handle to the containing directory remains open, even when that handle grants
/// `FILE_SHARE_DELETE`.  Long-lived handles would not prevent pathname rebinding anyway because
/// delete sharing deliberately permits rename.  Every mutation boundary therefore reopens both
/// paths, verifies the same file IDs and the private directory custody, performs the operation,
/// and re-reads them.  Ancestors above the target parent remain pinned as additional evidence.
/// The directory is intentionally not removed by `Drop`: a crash or indeterminate publication
/// must leave its journal and previous image.
#[derive(Debug)]
pub struct SecurePublishSession {
    path: PathBuf,
    session_id: SessionId,
    directory_identity: FileId,
    parent_identity: FileId,
    _pins: PinnedDirectoryAncestors,
}

/// Move-only authorization for replacing/appending one exact existing image. The old-image
/// handle refuses write sharing, allows the delete sharing needed by WinPE handle rename, and
/// remains alive from the copy through the CAS rename.
pub struct ExistingBackupPreparation {
    old: File,
    identity: FileIdentity,
    staged_identity: FileIdentity,
    staged_copy_retained: bool,
    session_directory_id: FileId,
    target_name: String,
    staged_extension: String,
}

impl ExistingBackupPreparation {
    pub fn old_expectation(&self) -> FileExpectation {
        self.identity.expectation
    }
}

impl SecurePublishSession {
    pub fn create(target_parent: &Path, session_id: &SessionId) -> Result<Self> {
        let pins = pin_existing_parent_directory_ancestors(target_parent)
            .context("pin backup publish target parent")?;
        pins.verify_unchanged()
            .context("verify backup publish target parent pins")?;
        let parent = open_directory_locked(target_parent)
            .with_context(|| format!("open backup target parent {}", target_parent.display()))?;
        let prefix = format!("LetRecovery-BackupPublish-{}", session_id.as_str());
        let guard = ScopedTempDir::create_system_administrators_in(target_parent, &prefix)
            .context("create secure backup publish session")?;
        guard.verify_system_administrators_custody()?;
        // Transfer and verify the exact create-time handle, record its identity, then close both
        // directory handles before any child namespace operation. `verify_pins` re-establishes
        // those exact identities at every boundary.
        let (path, directory) = guard
            .into_system_administrators_directory()
            .context("transfer newly created backup publish session custody")?;
        verify_system_administrators_directory_custody(&directory)?;
        let parent_identity = file_id(&parent)?;
        let directory_identity = file_id(&directory)?;
        if parent_identity.volume != directory_identity.volume {
            bail!("backup publish session is not on the target volume");
        }
        pins.verify_unchanged()
            .context("verify target parent after creating backup publish session")?;
        if file_id(&directory)? != directory_identity {
            bail!("secure backup publish session identity changed during custody transfer");
        }
        drop(directory);
        drop(parent);
        Ok(Self {
            path,
            session_id: session_id.clone(),
            directory_identity,
            parent_identity,
            _pins: pins,
        })
    }

    /// Reopen a session left by a prior crash. Its strict journal and file custody are revalidated
    /// by `recover_*`; merely opening a matching directory name never authorizes mutation.
    pub fn open(target_parent: &Path, session_id: &SessionId) -> Result<Self> {
        Self::open_if_present(target_parent, session_id)?
            .ok_or_else(|| anyhow!("backup publish recovery found no matching private session"))
    }

    /// Open the one crash-recovery session for this authenticated operation, if it exists.
    ///
    /// Absence is the only condition returned as `None`. Multiple names, a reparse point,
    /// unexpected custody or an identity mismatch remain hard errors so callers cannot silently
    /// start a second publication over an indeterminate transaction.
    pub fn open_if_present(target_parent: &Path, session_id: &SessionId) -> Result<Option<Self>> {
        let pins = pin_existing_parent_directory_ancestors(target_parent)
            .context("pin backup publish target parent")?;
        let parent = open_directory_locked(target_parent)?;
        let publish_prefix = format!("LetRecovery-BackupPublish-{}-", session_id.as_str());
        let committed_prefix = format!("LetRecovery-BackupCommitted-{}-", session_id.as_str());
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(target_parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("backup publish parent contains a non-Unicode entry"))?;
            if name.starts_with(&publish_prefix) || name.starts_with(&committed_prefix) {
                candidates.push(entry);
            }
        }
        if candidates.is_empty() {
            pins.verify_unchanged()?;
            return Ok(None);
        }
        if candidates.len() != 1 {
            bail!("backup publish recovery requires exactly one matching private session");
        }
        let path = candidates[0].path();
        let directory = open_directory_locked(&path)?;
        verify_system_administrators_directory_custody(&directory)
            .context("backup publish recovery directory custody verification failed")?;
        let parent_identity = file_id(&parent)?;
        let directory_identity = file_id(&directory)?;
        if parent_identity.volume != directory_identity.volume {
            bail!("backup publish recovery session is not on the target volume");
        }
        pins.verify_unchanged()?;
        drop(directory);
        drop(parent);
        Ok(Some(Self {
            path,
            session_id: session_id.clone(),
            directory_identity,
            parent_identity,
            _pins: pins,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a staged image with custody installed by CREATE_NEW itself. The returned writer must
    /// be flushed and closed before publication; publication reopens the exact name with DELETE
    /// access, refuses write sharing and allows the delete sharing required by WinPE rename for
    /// the complete CAS transaction.
    pub fn create_staged_file(&self, extension: &str) -> Result<(PathBuf, File)> {
        validate_extension(extension)?;
        let path = self.path.join(format!("staged.{extension}"));
        let file = create_system_administrators_file_new(&path)
            .with_context(|| format!("create secure staged backup {}", path.display()))?;
        Ok((path, file))
    }

    /// Reopen and hash the completed staged image while refusing concurrent writes/deletes.
    /// Publication repeats the same observation and requires an exact match, so a caller never
    /// has to derive its immutable-byte expectation through an unlocked pathname read.
    pub fn inspect_staged_file(&self, extension: &str) -> Result<FileExpectation> {
        validate_extension(extension)?;
        self.verify_pins()?;
        let mut file = open_staged_file_locked(&self.staged_path(extension))
            .context("lock completed staged backup for inspection")?;
        verify_system_administrators_file_custody(&file)
            .context("completed staged backup custody verification failed")?;
        let identity = observe(&mut file)?;
        if identity.id.volume != self.parent_identity.volume {
            bail!("completed staged backup is not on the target volume");
        }
        self.verify_pins()?;
        Ok(identity.expectation)
    }

    pub fn prepare_existing_copy(
        &self,
        target_name: &str,
        staged_extension: &str,
    ) -> Result<ExistingBackupPreparation> {
        self.prepare_existing_copy_impl(target_name, staged_extension, true)
    }

    /// Inspect the exact staged copy produced for an append while holding a read handle that
    /// denies writers and deletion. The copied file ID and bytes are checked both before and after
    /// the callback, and the original public target remains locked by the move-only preparation.
    pub fn inspect_prepared_staged<T>(
        &self,
        preparation: &mut ExistingBackupPreparation,
        inspect: impl FnOnce(&Path) -> Result<T>,
    ) -> Result<T> {
        self.inspect_prepared_staged_impl(preparation, inspect, true)
    }

    fn inspect_prepared_staged_impl<T>(
        &self,
        preparation: &mut ExistingBackupPreparation,
        inspect: impl FnOnce(&Path) -> Result<T>,
        enforce_custody: bool,
    ) -> Result<T> {
        self.verify_pins()?;
        if self.directory_identity != preparation.session_directory_id {
            bail!("existing backup preparation belongs to another private session");
        }
        if !preparation.staged_copy_retained {
            bail!("existing backup preparation no longer retains its staged copy");
        }
        let old = observe_exact(&mut preparation.old, preparation.identity.expectation)?;
        if old != preparation.identity {
            bail!("locked existing backup changed before staged catalog inspection");
        }
        let path = self.staged_path(&preparation.staged_extension);
        let mut staged = open_file_read_pinned(&path)
            .context("pin copied staged backup for semantic inspection")?;
        if enforce_custody {
            verify_system_administrators_file_custody(&staged)
                .context("copied staged backup custody verification failed")?;
        }
        let before = observe_exact(&mut staged, preparation.staged_identity.expectation)?;
        if before != preparation.staged_identity {
            bail!("copied staged backup identity changed before semantic inspection");
        }
        let value = inspect(&path)?;
        let after = observe_exact(&mut staged, preparation.staged_identity.expectation)?;
        if after != preparation.staged_identity {
            bail!("copied staged backup changed during semantic inspection");
        }
        self.verify_pins()?;
        Ok(value)
    }

    fn prepare_existing_copy_impl(
        &self,
        target_name: &str,
        staged_extension: &str,
        enforce_custody: bool,
    ) -> Result<ExistingBackupPreparation> {
        validate_component(target_name)?;
        validate_extension(staged_extension)?;
        validate_matching_target_extension(target_name, staged_extension)?;
        self.verify_pins()?;
        reject_existing(&self.previous_path(), "previous image")?;
        reject_existing(&self.journal_path(), "publish journal")?;
        let mut old = open_file_locked(&self.target_path(target_name)?)?;
        let identity = observe(&mut old)?;
        if identity.id.volume != self.parent_identity.volume {
            bail!("existing backup is not on the target volume");
        }
        let path = self.staged_path(staged_extension);
        let mut staged = if enforce_custody {
            create_system_administrators_file_new(&path)
                .with_context(|| format!("create secure staged backup {}", path.display()))?
        } else {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)?
        };
        old.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut old, &mut staged)?;
        staged.flush()?;
        flush_file(&staged)?;
        drop(staged);
        let mut copied = open_staged_file_locked(&path)
            .context("lock secure staged copy for identity binding")?;
        if enforce_custody {
            verify_system_administrators_file_custody(&copied)
                .context("secure staged copy custody verification failed")?;
        }
        let copied_identity = observe_exact(&mut copied, identity.expectation)?;
        if copied_identity.id.volume != identity.id.volume {
            bail!("secure staged copy does not exactly match the locked existing image");
        }
        drop(copied);
        Ok(ExistingBackupPreparation {
            old,
            identity,
            staged_identity: copied_identity,
            staged_copy_retained: true,
            session_directory_id: self.directory_identity,
            target_name: target_name.to_owned(),
            staged_extension: staged_extension.to_owned(),
        })
    }

    /// Delete only the exact copy produced by `prepare_existing_copy`, while retaining the
    /// move-only old-image lock for a replacement capture. A caller may then construct a complete
    /// fresh image in the fixed staged slot and hand this preparation to `publish_existing`.
    pub fn discard_copied_staged_for_replace(
        &self,
        preparation: &mut ExistingBackupPreparation,
    ) -> Result<()> {
        self.discard_copied_staged_for_replace_impl(preparation, true)
    }

    fn discard_copied_staged_for_replace_impl(
        &self,
        preparation: &mut ExistingBackupPreparation,
        enforce_custody: bool,
    ) -> Result<()> {
        self.verify_pins()?;
        if self.directory_identity != preparation.session_directory_id {
            bail!("existing backup preparation belongs to another private session");
        }
        let old = observe_exact(&mut preparation.old, preparation.identity.expectation)?;
        if old != preparation.identity {
            bail!("locked existing backup changed before replacement capture");
        }
        let staged_path = self.staged_path(&preparation.staged_extension);
        let mut staged = open_staged_file_locked(&staged_path)
            .context("lock copied staged backup for replacement reset")?;
        if enforce_custody {
            verify_system_administrators_file_custody(&staged)
                .context("copied staged backup custody verification failed")?;
        }
        if observe_exact(&mut staged, preparation.staged_identity.expectation)?
            != preparation.staged_identity
        {
            bail!("copied staged backup identity changed before replacement reset");
        }
        delete_on_close(&staged)?;
        drop(staged);
        reject_existing(&staged_path, "discarded staged copy")?;
        preparation.staged_copy_retained = false;
        self.verify_pins()?;
        Ok(())
    }

    /// Remove a successfully drained session through a freshly rebound exact directory handle.
    ///
    /// Any unexpected entry makes this fail closed; callers must never recursively delete a CAS
    /// recovery directory merely because the main operation succeeded.
    pub fn remove_empty(self) -> Result<()> {
        self.verify_pins()?;
        let directory = open_directory_locked(&self.path)
            .context("reopen empty backup publish session for exact deletion")?;
        if file_id(&directory)? != self.directory_identity {
            bail!("empty backup publish session pathname rebound before deletion");
        }
        verify_system_administrators_directory_custody(&directory)?;
        {
            let mut entries = std::fs::read_dir(&self.path)?;
            if entries.next().transpose()?.is_some() {
                bail!(
                    "backup publish session is not empty; preserving it for diagnosis or recovery"
                );
            }
        }
        // `FileDispositionInfo` removes the directory only after its last handle closes. Keep the
        // emptiness check in a separate scope so the `ReadDir` enumeration handle is closed before
        // marking and closing the exact DELETE-capable directory handle. Otherwise deletion can
        // remain pending and the authoritative pathname readback below must correctly reject it.
        let Self { path, _pins, .. } = self;
        delete_on_close(&directory)?;
        drop(directory);
        if path.exists() {
            bail!("backup publish session directory remained after handle deletion");
        }
        _pins.verify_unchanged()?;
        Ok(())
    }

    fn target_path(&self, target_name: &str) -> Result<PathBuf> {
        validate_component(target_name)?;
        Ok(self
            .path
            .parent()
            .ok_or_else(|| anyhow!("backup publish session has no parent"))?
            .join(target_name))
    }

    fn previous_path(&self) -> PathBuf {
        self.path.join("previous.image")
    }

    fn staged_path(&self, extension: &str) -> PathBuf {
        self.path.join(format!("staged.{extension}"))
    }

    fn journal_path(&self) -> PathBuf {
        self.path.join("publish.journal")
    }

    fn committed_receipt(&self) -> Result<Option<(PublishMode, FileIdentity)>> {
        let name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("backup publish session name is not Unicode"))?;
        let prefix = format!("LetRecovery-BackupCommitted-{}-", self.session_id.as_str());
        let Some(fields) = name.strip_prefix(&prefix) else {
            return Ok(None);
        };
        let parts = fields.split('-').collect::<Vec<_>>();
        if parts.len() != 5 {
            bail!("committed backup receipt name has invalid field count");
        }
        let mode = PublishMode::parse(parts[0])?;
        let length = parse_canonical_u64(parts[1], "committed length")?;
        let sha256 = decode_hash(parts[2])?;
        if parts[3].len() != 8
            || parts[4].len() != 16
            || !parts[3]
                .bytes()
                .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
            || !parts[4]
                .bytes()
                .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
        {
            bail!("committed backup receipt contains a non-canonical file ID");
        }
        let volume = u32::from_str_radix(parts[3], 16)?;
        let index = u64::from_str_radix(parts[4], 16)?;
        Ok(Some((
            mode,
            FileIdentity {
                id: FileId { volume, index },
                expectation: FileExpectation { length, sha256 },
            },
        )))
    }

    fn mark_committed(
        &mut self,
        mode: PublishMode,
        identity: FileIdentity,
        enforce_custody: bool,
    ) -> Result<()> {
        let name = format!(
            "LetRecovery-BackupCommitted-{}-{}-{}-{}-{:08x}-{:016x}",
            self.session_id.as_str(),
            mode.as_str(),
            identity.expectation.length,
            encode_hex(&identity.expectation.sha256),
            identity.id.volume,
            identity.id.index,
        );
        let path = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("backup publish session has no parent"))?
            .join(&name);
        self.verify_pins()?;
        let mut directory = open_directory_locked(&self.path)
            .context("reopen backup publish session for committed receipt rename")?;
        if file_id(&directory)? != self.directory_identity {
            bail!("backup publish session pathname rebound before committed receipt rename");
        }
        if enforce_custody {
            verify_system_administrators_directory_custody(&directory)?;
        }
        let mut directory_moved = false;
        rename_handle_no_replace(&mut directory, &self.path, &path, &mut directory_moved)
            .context("atomically publish the committed backup receipt directory name")?;
        self.path = path;
        if file_id(&directory)? != self.directory_identity {
            bail!("committed backup receipt rename changed the private directory identity");
        }
        drop(directory);
        self.verify_pins()?;
        Ok(())
    }

    fn verify_pins(&self) -> Result<()> {
        self._pins.verify_unchanged()?;
        if self.parent_identity.volume != self.directory_identity.volume {
            bail!("backup publish session volume identity changed");
        }
        let parent_path = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("backup publish session has no target parent"))?;
        let current_parent = open_directory_readback(parent_path)
            .context("reopen backup publish target parent for identity readback")?;
        if file_id(&current_parent)? != self.parent_identity {
            bail!(
                "backup publish target parent pathname no longer identifies its retained directory"
            );
        }
        let current = open_directory_readback(&self.path)
            .context("reopen backup publish session for identity readback")?;
        if file_id(&current)? != self.directory_identity {
            bail!("backup publish session pathname no longer identifies its retained directory");
        }
        Ok(())
    }

    fn open_verified_directory_custody(&self) -> Result<File> {
        self.verify_pins()?;
        let directory = open_directory_locked(&self.path)
            .context("reopen backup publish session for custody verification")?;
        if file_id(&directory)? != self.directory_identity {
            bail!("backup publish session pathname rebound during custody verification");
        }
        verify_system_administrators_directory_custody(&directory)
            .context("backup publish session custody verification failed")?;
        Ok(directory)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryOutcome {
    Committed,
    RolledBack,
}

/// Hash one existing public backup while retaining a handle that refuses concurrent writes and
/// deletes for the complete observation. Callers persist only the length/SHA expectation; the
/// production preparation repeats the observation and retains its own handle through CAS.
pub fn inspect_existing_file(path: &Path) -> Result<FileExpectation> {
    let mut file = open_file_locked(path)
        .with_context(|| format!("lock existing backup {}", path.display()))?;
    Ok(observe(&mut file)?.expectation)
}

pub fn publish_existing(
    session: &mut SecurePublishSession,
    kind: ExistingPublishKind,
    preparation: ExistingBackupPreparation,
    new: FileExpectation,
) -> Result<()> {
    let mode = kind.mode();
    if session.directory_identity != preparation.session_directory_id {
        bail!("existing backup preparation belongs to another private session");
    }
    match (kind, preparation.staged_copy_retained) {
        (ExistingPublishKind::Append, true) | (ExistingPublishKind::Replace, false) => {}
        (ExistingPublishKind::Append, false) => {
            bail!("append publication lost its copied staged-image object")
        }
        (ExistingPublishKind::Replace, true) => {
            bail!("replace publication did not reset the copied staged-image slot")
        }
    }
    let target_name = preparation.target_name.clone();
    let staged_extension = preparation.staged_extension.clone();
    let old = preparation.identity.expectation;
    if kind == ExistingPublishKind::Append {
        let mut staged = open_staged_file_locked(&session.staged_path(&staged_extension))?;
        verify_system_administrators_file_custody(&staged)?;
        let identity = observe_exact(&mut staged, new)?;
        if identity.id != preparation.staged_identity.id {
            bail!("append staging no longer identifies the copied image object");
        }
    }
    publish_impl(
        session,
        PublishSpec {
            mode,
            target_name: &target_name,
            staged_extension: &staged_extension,
            expected_old: Some(old),
            expected_new: new,
            enforce_custody: true,
        },
        Some((preparation.old, preparation.identity)),
        &NoFault,
    )
}

pub fn publish_create(
    session: &mut SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
    new: FileExpectation,
) -> Result<()> {
    publish_impl(
        session,
        PublishSpec {
            mode: PublishMode::Create,
            target_name,
            staged_extension,
            expected_old: None,
            expected_new: new,
            enforce_custody: true,
        },
        None,
        &NoFault,
    )
}

pub fn recover(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
) -> Result<RecoveryOutcome> {
    recover_impl(session, target_name, staged_extension, None, true)
}

/// Recover only a create-only publication. A journal from replace/append is rejected even when
/// its session and path fields otherwise match the request.
pub fn recover_create(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
) -> Result<RecoveryOutcome> {
    recover_create_impl(session, target_name, staged_extension, true)
}

/// Recover only the requested replace/append publication. Before PREPARED, the exact staged-copy
/// slot may be removed while the still-existing target is held and verified as an ordinary
/// same-volume file. After PREPARED, recovery is driven by the journal's old/new file identities.
pub fn recover_existing(
    session: &SecurePublishSession,
    kind: ExistingPublishKind,
    target_name: &str,
    staged_extension: &str,
) -> Result<RecoveryOutcome> {
    recover_existing_impl(session, kind.mode(), target_name, staged_extension, true)
}

fn recover_create_impl(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
    enforce_custody: bool,
) -> Result<RecoveryOutcome> {
    match std::fs::symlink_metadata(session.journal_path()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some((mode, receipt)) = session.committed_receipt()? {
                if mode != PublishMode::Create {
                    bail!("committed backup receipt mode does not match create recovery");
                }
                let target = classify_slot(
                    &session.target_path(target_name)?,
                    None,
                    receipt,
                    enforce_custody,
                )?;
                if !matches!(target, Slot::New(_)) {
                    bail!("committed backup receipt does not match the live target object");
                }
                if std::fs::read_dir(session.path())?
                    .next()
                    .transpose()?
                    .is_some()
                {
                    bail!("committed backup receipt directory contains an unexpected object");
                }
                session.verify_pins()?;
                return Ok(RecoveryOutcome::Committed);
            }
            rollback_unprepared_create(session, target_name, staged_extension, enforce_custody)?;
            return Ok(RecoveryOutcome::RolledBack);
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() || is_reparse(&metadata) => {
            bail!("backup publish recovery journal is not an ordinary file")
        }
        Ok(_) => {}
    }
    recover_impl(
        session,
        target_name,
        staged_extension,
        Some(PublishMode::Create),
        enforce_custody,
    )
}

fn recover_existing_impl(
    session: &SecurePublishSession,
    mode: PublishMode,
    target_name: &str,
    staged_extension: &str,
    enforce_custody: bool,
) -> Result<RecoveryOutcome> {
    if !matches!(mode, PublishMode::Replace | PublishMode::Append) {
        bail!("existing backup recovery requires replace or append mode");
    }
    match std::fs::symlink_metadata(session.journal_path()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some((receipt_mode, receipt)) = session.committed_receipt()? {
                if receipt_mode != mode {
                    bail!("committed backup receipt mode does not match existing recovery");
                }
                let target = classify_slot(
                    &session.target_path(target_name)?,
                    None,
                    receipt,
                    enforce_custody,
                )?;
                if !matches!(target, Slot::New(_)) {
                    bail!("committed backup receipt does not match the live target object");
                }
                if std::fs::read_dir(session.path())?
                    .next()
                    .transpose()?
                    .is_some()
                {
                    bail!("committed backup receipt directory contains an unexpected object");
                }
                session.verify_pins()?;
                return Ok(RecoveryOutcome::Committed);
            }
            rollback_unprepared_existing(session, target_name, staged_extension, enforce_custody)?;
            Ok(RecoveryOutcome::RolledBack)
        }
        Err(error) => Err(error.into()),
        Ok(metadata) if !metadata.is_file() || is_reparse(&metadata) => {
            bail!("backup publish recovery journal is not an ordinary file")
        }
        Ok(_) => recover_impl(
            session,
            target_name,
            staged_extension,
            Some(mode),
            enforce_custody,
        ),
    }
}

fn rollback_unprepared_existing(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
    enforce_custody: bool,
) -> Result<()> {
    validate_component(target_name)?;
    validate_extension(staged_extension)?;
    validate_matching_target_extension(target_name, staged_extension)?;
    session.verify_pins()?;
    if enforce_custody {
        drop(session.open_verified_directory_custody()?);
    }
    reject_existing(&session.previous_path(), "unprepared previous image")?;
    reject_existing(&session.journal_path(), "unprepared journal")?;

    let mut target = open_file_locked(&session.target_path(target_name)?)
        .context("lock existing backup during unprepared-session cleanup")?;
    let target_identity = observe(&mut target)?;
    if target_identity.id.volume != session.parent_identity.volume {
        bail!("unprepared existing backup target is not on the target volume");
    }

    let staged_path = session.staged_path(staged_extension);
    let expected_name = staged_path
        .file_name()
        .ok_or_else(|| anyhow!("unprepared staged slot has no filename"))?;
    let mut staged_present = false;
    for entry in std::fs::read_dir(session.path())? {
        let entry = entry?;
        if entry.file_name() != expected_name || staged_present {
            bail!("unprepared backup session contains an unexpected object; preserving it");
        }
        staged_present = true;
    }
    if staged_present {
        let staged = open_staged_file_locked(&staged_path)
            .context("lock unprepared staged copy for exact cleanup")?;
        let metadata = staged.metadata()?;
        if !metadata.is_file() || is_reparse(&metadata) {
            bail!("unprepared staged backup is not an ordinary non-reparse file");
        }
        if enforce_custody {
            verify_system_administrators_file_custody(&staged)?;
        }
        if file_id(&staged)?.volume != target_identity.id.volume {
            bail!("unprepared staged backup is not on the target volume");
        }
        delete_on_close(&staged)?;
        drop(staged);
        reject_existing(&staged_path, "discarded unprepared staged backup")?;
    }
    drop(target);
    session.verify_pins()?;
    Ok(())
}

/// Remove only an uncommitted create session that never reached PREPARED.
///
/// The authenticated target name must still be absent, and the private session may contain only
/// the one fixed staged-file slot. Any extra object, previous slot or journal is indeterminate and
/// is preserved. The staged pathname is opened with DELETE access while refusing write sharing,
/// checked as an ordinary same-volume file, then removed through that exact file-ID-bound handle.
fn rollback_unprepared_create(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
    enforce_custody: bool,
) -> Result<()> {
    validate_component(target_name)?;
    validate_extension(staged_extension)?;
    validate_matching_target_extension(target_name, staged_extension)?;
    session.verify_pins()?;
    if enforce_custody {
        drop(session.open_verified_directory_custody()?);
    }
    reject_existing(
        &session.target_path(target_name)?,
        "unprepared create target",
    )?;
    reject_existing(&session.previous_path(), "unprepared previous image")?;
    reject_existing(&session.journal_path(), "unprepared journal")?;

    let staged_path = session.staged_path(staged_extension);
    let expected_name = staged_path
        .file_name()
        .ok_or_else(|| anyhow!("unprepared staged slot has no filename"))?;
    let mut staged_present = false;
    for entry in std::fs::read_dir(session.path())? {
        let entry = entry?;
        if entry.file_name() != expected_name || staged_present {
            bail!("unprepared backup session contains an unexpected object; preserving it");
        }
        staged_present = true;
    }
    if staged_present {
        let file = open_staged_file_locked(&staged_path)
            .context("lock unprepared staged backup for exact cleanup")?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse(&metadata) {
            bail!("unprepared staged backup is not an ordinary non-reparse file");
        }
        if file_id(&file)?.volume != session.parent_identity.volume {
            bail!("unprepared staged backup is not on the target volume");
        }
        delete_on_close(&file)?;
        drop(file);
        if staged_path.exists() {
            bail!("unprepared staged backup remained after exact handle deletion");
        }
    }
    session.verify_pins()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileId {
    volume: u32,
    index: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    id: FileId,
    expectation: FileExpectation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Journal {
    mode: PublishMode,
    session_id: String,
    target_name: String,
    staged_extension: String,
    old: Option<FileIdentity>,
    new: FileIdentity,
}

impl Journal {
    fn encode(&self) -> Vec<u8> {
        let (old_length, old_hash, old_volume, old_index) = match self.old {
            Some(old) => (
                old.expectation.length.to_string(),
                encode_hex(&old.expectation.sha256),
                old.id.volume.to_string(),
                old.id.index.to_string(),
            ),
            None => ("-".into(), "-".into(), "-".into(), "-".into()),
        };
        let body = format!(
            "{JOURNAL_MAGIC}\r\nState=PREPARED\r\nMode={}\r\nSessionId={}\r\nTargetName={}\r\nStagedExtension={}\r\nOldLength={old_length}\r\nOldSha256={old_hash}\r\nOldVolume={old_volume}\r\nOldFileId={old_index}\r\nNewLength={}\r\nNewSha256={}\r\nNewVolume={}\r\nNewFileId={}\r\n",
            self.mode.as_str(),
            self.session_id,
            self.target_name,
            self.staged_extension,
            self.new.expectation.length,
            encode_hex(&self.new.expectation.sha256),
            self.new.id.volume,
            self.new.id.index,
        );
        let checksum = Sha256::digest(body.as_bytes());
        format!("{body}Checksum={}\r\n", encode_hex(&checksum)).into_bytes()
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > JOURNAL_MAX_BYTES {
            bail!("backup publish journal is empty or exceeds its byte limit");
        }
        let text = std::str::from_utf8(bytes).context("backup publish journal is not UTF-8")?;
        if !text.ends_with("\r\n")
            || text.contains('\0')
            || text.contains('\n') && text.contains("\n\n")
        {
            bail!("backup publish journal has non-canonical framing");
        }
        let checksum_line_start = text
            .rfind("Checksum=")
            .ok_or_else(|| anyhow!("backup publish journal omits checksum"))?;
        let body = &text[..checksum_line_start];
        let checksum_text = text[checksum_line_start..]
            .strip_prefix("Checksum=")
            .and_then(|value| value.strip_suffix("\r\n"))
            .ok_or_else(|| anyhow!("backup publish journal checksum is malformed"))?;
        let checksum = decode_hash(checksum_text)?;
        if Sha256::digest(body.as_bytes()).as_slice() != checksum {
            bail!("backup publish journal checksum mismatch");
        }
        let lines = body
            .strip_suffix("\r\n")
            .ok_or_else(|| anyhow!("backup publish journal body is not canonical"))?
            .split("\r\n")
            .collect::<Vec<_>>();
        if lines.len() != 14 || lines[0] != JOURNAL_MAGIC || lines[1] != "State=PREPARED" {
            bail!("backup publish journal schema is not canonical");
        }
        let value = |index: usize, key: &str| -> Result<&str> {
            lines[index]
                .strip_prefix(key)
                .ok_or_else(|| anyhow!("backup publish journal field order mismatch"))
        };
        let mode = PublishMode::parse(value(2, "Mode=")?)?;
        let session_id = value(3, "SessionId=")?;
        SessionId::parse(session_id)?;
        let target_name = value(4, "TargetName=")?;
        validate_component(target_name)?;
        let staged_extension = value(5, "StagedExtension=")?;
        validate_extension(staged_extension)?;
        validate_matching_target_extension(target_name, staged_extension)?;
        let old_fields = (
            value(6, "OldLength=")?,
            value(7, "OldSha256=")?,
            value(8, "OldVolume=")?,
            value(9, "OldFileId=")?,
        );
        let old = if old_fields == ("-", "-", "-", "-") {
            None
        } else {
            Some(parse_identity(old_fields)?)
        };
        if (mode == PublishMode::Create) != old.is_none() {
            bail!("backup publish journal old identity does not match its mode");
        }
        let new = parse_identity((
            value(10, "NewLength=")?,
            value(11, "NewSha256=")?,
            value(12, "NewVolume=")?,
            value(13, "NewFileId=")?,
        ))?;
        Ok(Self {
            mode,
            session_id: session_id.to_owned(),
            target_name: target_name.to_owned(),
            staged_extension: staged_extension.to_owned(),
            old,
            new,
        })
    }
}

fn parse_identity(fields: (&str, &str, &str, &str)) -> Result<FileIdentity> {
    let length = parse_canonical_u64(fields.0, "file length")?;
    let sha256 = decode_hash(fields.1)?;
    let volume_u64 = parse_canonical_u64(fields.2, "volume serial")?;
    let volume = u32::try_from(volume_u64).context("volume serial exceeds u32")?;
    let index = parse_canonical_u64(fields.3, "file id")?;
    if length == 0 || index == 0 {
        bail!("backup publish journal contains a zero length or file id");
    }
    Ok(FileIdentity {
        id: FileId { volume, index },
        expectation: FileExpectation { length, sha256 },
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    PreparedPersisted,
    OldMoved,
    NewPublished,
}

trait FaultInjector {
    fn hit(&self, _point: FaultPoint, _session: &SecurePublishSession) -> Result<()> {
        Ok(())
    }
}

struct NoFault;
impl FaultInjector for NoFault {}

struct PublishSpec<'a> {
    mode: PublishMode,
    target_name: &'a str,
    staged_extension: &'a str,
    expected_old: Option<FileExpectation>,
    expected_new: FileExpectation,
    enforce_custody: bool,
}

fn publish_impl(
    session: &mut SecurePublishSession,
    specification: PublishSpec<'_>,
    prepared_old: Option<(File, FileIdentity)>,
    faults: &dyn FaultInjector,
) -> Result<()> {
    let PublishSpec {
        mode,
        target_name,
        staged_extension,
        expected_old,
        expected_new,
        enforce_custody,
    } = specification;
    validate_component(target_name)?;
    validate_extension(staged_extension)?;
    validate_matching_target_extension(target_name, staged_extension)?;
    if expected_new.length == 0 || expected_old.is_some_and(|old| old.length == 0) {
        bail!("backup publish expectations must have non-zero lengths");
    }
    if (mode == PublishMode::Create) != expected_old.is_none() {
        bail!("backup publish mode and old expectation disagree");
    }
    session.verify_pins()?;
    if enforce_custody {
        drop(session.open_verified_directory_custody()?);
    }
    let target_path = session.target_path(target_name)?;
    let staged_path = session.staged_path(staged_extension);
    let previous_path = session.previous_path();
    let journal_path = session.journal_path();
    reject_existing(&previous_path, "previous image")?;
    reject_existing(&journal_path, "publish journal")?;

    let mut staged = open_staged_file_locked(&staged_path)
        .with_context(|| format!("lock staged backup {}", staged_path.display()))?;
    if enforce_custody {
        verify_system_administrators_file_custody(&staged)
            .context("staged backup custody verification failed")?;
    }
    flush_file(&staged).context("flush staged backup before publication")?;
    let staged_identity = observe_exact(&mut staged, expected_new)
        .context("staged backup does not match the completed-image expectation")?;
    if staged_identity.id.volume != session.parent_identity.volume {
        bail!("staged backup is not on the target volume");
    }

    let mut old = match (expected_old, prepared_old) {
        (Some(expectation), Some((mut file, identity))) => {
            if identity.expectation != expectation
                || observe_exact(&mut file, expectation)? != identity
            {
                bail!("locked existing backup changed after secure staged copy");
            }
            if identity.id.volume != staged_identity.id.volume {
                bail!("existing and staged backups are not on the same volume");
            }
            Some((file, identity))
        }
        (Some(expectation), None) => {
            let mut file = open_file_locked(&target_path)
                .with_context(|| format!("lock existing backup {}", target_path.display()))?;
            let identity = observe_exact(&mut file, expectation)
                .context("existing backup changed before publication")?;
            if identity.id.volume != staged_identity.id.volume {
                bail!("existing and staged backups are not on the same volume");
            }
            if identity.id == staged_identity.id {
                bail!("existing and staged backup names identify the same file object");
            }
            Some((file, identity))
        }
        (None, None) => {
            reject_existing(&target_path, "create target")?;
            None
        }
        (None, Some(_)) => bail!("create publication cannot carry an existing-image handle"),
    };

    let journal = Journal {
        mode,
        session_id: session.session_id.as_str().to_owned(),
        target_name: target_name.to_owned(),
        staged_extension: staged_extension.to_owned(),
        old: old.as_ref().map(|(_, identity)| *identity),
        new: staged_identity,
    };
    let journal_bytes = journal.encode();
    if journal_bytes.len() > JOURNAL_MAX_BYTES {
        bail!("backup publish journal exceeds its byte limit");
    }
    let mut journal_writer = if enforce_custody {
        create_system_administrators_file_new(&journal_path)
            .context("create secure PREPARED backup publish journal")?
    } else {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&journal_path)?
    };
    journal_writer.write_all(&journal_bytes)?;
    journal_writer.flush()?;
    flush_file(&journal_writer).context("durably flush PREPARED backup publish journal")?;
    drop(journal_writer);
    let journal_file = open_file_locked(&journal_path)?;
    if enforce_custody {
        verify_system_administrators_file_custody(&journal_file)?;
    }
    session.verify_pins()?;
    faults.hit(FaultPoint::PreparedPersisted, session)?;

    let mut old_moved = false;
    let mut new_published = false;
    let operation = (|| -> Result<()> {
        if let Some((ref mut old_file, old_identity)) = old {
            rename_handle_no_replace(old_file, &target_path, &previous_path, &mut old_moved)
                .context("move existing backup into the private previous slot")?;
            if observe_exact(old_file, old_identity.expectation)? != old_identity {
                bail!("previous backup changed across its publication rename boundary");
            }
            session.verify_pins()?;
            faults.hit(FaultPoint::OldMoved, session)?;
        }
        rename_handle_no_replace(&mut staged, &staged_path, &target_path, &mut new_published)
            .context("publish staged backup into the target slot")?;
        session.verify_pins()?;
        faults.hit(FaultPoint::NewPublished, session)?;
        let observed_new = observe_exact(&mut staged, expected_new)?;
        if observed_new != staged_identity {
            bail!("published backup handle identity changed");
        }
        Ok(())
    })();

    if let Err(error) = operation {
        let rollback = rollback_live(
            session,
            target_name,
            staged_extension,
            old.as_mut().map(|(file, _)| file),
            &mut staged,
            old_moved,
            new_published,
        );
        return match rollback {
            Ok(()) => {
                drop(old);
                drop(staged);
                drop(journal_file);
                match cleanup_path_if_exact(&journal_path, Some(&journal)) {
                    Ok(()) => Err(error).context("backup publication failed and was rolled back"),
                    Err(cleanup_error) => Err(anyhow!(
                        "backup publication failed and was rolled back, but its exact PREPARED journal could not be removed: {cleanup_error:#}; original failure: {error:#}"
                    )),
                }
            }
            Err(rollback_error) => Err(anyhow!(
                "backup publication failed: {error:#}; rollback could not be proven: {rollback_error:#}; previous image and PREPARED journal were preserved for recovery"
            )),
        };
    }

    // Close the PREPARED child handle before renaming its parent directory. Its canonical bytes
    // are reopened and revalidated below; a crash in between still leaves the original durable
    // journal discoverable under the uncommitted session name.
    drop(journal_file);
    // The locked old handle now names `previous.image`. Close it only after the new target has
    // been verified; the durable journal is sufficient for recovery from this point onward, and
    // no child handle may prevent the private directory from being renamed as the receipt.
    drop(old.take());

    // Rename the private directory itself to a durable, self-describing committed receipt before
    // deleting PREPARED. If the process dies after journal deletion but before directory cleanup,
    // recovery can still prove target=new by the receipt's exact hash and file ID.
    session.mark_committed(mode, staged_identity, enforce_custody)?;
    let mut journal_file = open_file_locked(&session.journal_path())?;
    if enforce_custody {
        verify_system_administrators_file_custody(&journal_file)?;
    }
    if Journal::parse(&read_bounded(&mut journal_file, JOURNAL_MAX_BYTES as u64)?)? != journal {
        bail!("backup publish PREPARED journal changed before commit cleanup");
    }

    // The new target is fully verified. Remove the previous image first. If the process crashes
    // before journal removal, recovery recognizes target=new with no previous/staged and commits.
    if let Some(old_identity) = journal.old {
        let current_previous_path = session.previous_path();
        let mut old_file = open_file_locked(&current_previous_path)
            .context("reopen previous backup after durable commit receipt")?;
        if observe_exact(&mut old_file, old_identity.expectation)? != old_identity {
            bail!("previous backup identity changed before committed cleanup");
        }
        delete_on_close(&old_file).context("mark previous backup for deletion after commit")?;
        drop(old_file);
        reject_existing(&current_previous_path, "committed previous backup")?;
    }
    delete_on_close(&journal_file).context("mark backup publish journal committed")?;
    drop(journal_file);
    reject_existing(&session.journal_path(), "committed backup publish journal")?;
    drop(staged);
    session.verify_pins()?;
    Ok(())
}

fn rollback_live(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
    old: Option<&mut File>,
    staged: &mut File,
    old_moved: bool,
    new_published: bool,
) -> Result<()> {
    if new_published {
        rollback_new_publication(
            staged,
            &session.target_path(target_name)?,
            &session.staged_path(staged_extension),
        )?;
    }
    if old_moved {
        let old = old.ok_or_else(|| anyhow!("rollback lost the existing-image handle"))?;
        let mut moved = false;
        rename_handle_no_replace(
            old,
            &session.previous_path(),
            &session.target_path(target_name)?,
            &mut moved,
        )
        .context("restore previous image to its original target during rollback")?;
    }
    session.verify_pins()?;
    Ok(())
}

#[derive(Debug)]
enum Slot {
    Missing,
    Old(File),
    New(File),
    Other,
}

fn recover_impl(
    session: &SecurePublishSession,
    target_name: &str,
    staged_extension: &str,
    expected_mode: Option<PublishMode>,
    enforce_custody: bool,
) -> Result<RecoveryOutcome> {
    session.verify_pins()?;
    let journal_path = session.journal_path();
    let mut journal_file =
        open_file_locked(&journal_path).context("lock backup publish journal")?;
    if enforce_custody {
        verify_system_administrators_file_custody(&journal_file)
            .context("backup publish journal custody verification failed")?;
    }
    let journal_bytes = read_bounded(&mut journal_file, JOURNAL_MAX_BYTES as u64)?;
    let journal = Journal::parse(&journal_bytes)?;
    if journal.session_id != session.session_id.as_str()
        || journal.target_name != target_name
        || journal.staged_extension != staged_extension
    {
        bail!("backup publish recovery request does not match its journal");
    }
    if expected_mode.is_some_and(|mode| journal.mode != mode) {
        bail!("backup publish recovery journal mode does not match the requested operation");
    }
    let target_path = session.target_path(target_name)?;
    let previous_path = session.previous_path();
    let staged_path = session.staged_path(staged_extension);
    let mut target = classify_slot(&target_path, journal.old, journal.new, false)?;
    let mut previous = classify_slot(&previous_path, journal.old, journal.new, false)?;
    let mut staged = classify_slot(&staged_path, journal.old, journal.new, enforce_custody)?;

    // The NTFS hard-link fallback can crash after atomically creating the public name but before
    // deleting the private staged name. Both names then identify the exact same verified file.
    // Treat that narrow, identity-proven state as an interrupted publication and roll it back;
    // different IDs remain indeterminate and are preserved.
    let duplicate_new_links = match (&target, &staged) {
        (Slot::New(target), Slot::New(staged)) => file_id(target)? == file_id(staged)?,
        _ => false,
    };

    let outcome = if duplicate_new_links {
        RecoveryOutcome::RolledBack
    } else {
        match (&target, &previous, &staged, journal.mode) {
        (Slot::New(_), Slot::Old(_), Slot::Missing, _)
        | (Slot::New(_), Slot::Missing, Slot::Missing, _) => RecoveryOutcome::Committed,
        (Slot::Old(_), Slot::Missing, Slot::New(_), PublishMode::Replace | PublishMode::Append)
        | (Slot::Missing, Slot::Old(_), Slot::New(_), PublishMode::Replace | PublishMode::Append)
        | (Slot::Missing, Slot::Missing, Slot::New(_), PublishMode::Create) => {
            RecoveryOutcome::RolledBack
        }
        _ => bail!(
            "backup publish recovery found an indeterminate or competing file set; previous image and journal were preserved"
        ),
        }
    };

    match outcome {
        RecoveryOutcome::Committed => {
            if let Slot::Old(file) = take_slot(&mut previous) {
                delete_on_close(&file)?;
                drop(file);
            }
        }
        RecoveryOutcome::RolledBack => {
            if duplicate_new_links {
                let public = match take_slot(&mut target) {
                    Slot::New(file) => file,
                    _ => unreachable!("duplicate hard-link state requires a public new slot"),
                };
                let private = match take_slot(&mut staged) {
                    Slot::New(file) => file,
                    _ => unreachable!("duplicate hard-link state requires a private new slot"),
                };
                staged = Slot::New(remove_public_hard_link_and_relock_private(
                    public,
                    private,
                    &target_path,
                    &staged_path,
                    journal.new.id,
                )?);
            }
            if matches!(target, Slot::Missing) {
                if let Slot::Old(mut file) = take_slot(&mut previous) {
                    let mut moved = false;
                    rename_handle_no_replace(
                        &mut file,
                        &previous_path,
                        &session.target_path(target_name)?,
                        &mut moved,
                    )?;
                    target = Slot::Old(file);
                }
            }
            if let Slot::New(file) = take_slot(&mut staged) {
                delete_on_close(&file)?;
                drop(file);
            }
        }
    }
    session.verify_pins()?;
    delete_on_close(&journal_file)?;
    drop(journal_file);
    if journal_path.exists() {
        bail!("backup publish recovery could not remove its journal");
    }
    drop(target);
    drop(previous);
    Ok(outcome)
}

fn take_slot(slot: &mut Slot) -> Slot {
    std::mem::replace(slot, Slot::Missing)
}

fn classify_slot(
    path: &Path,
    old: Option<FileIdentity>,
    new: FileIdentity,
    enforce_custody: bool,
) -> Result<Slot> {
    let mut file = match open_file_locked(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Slot::Missing),
        Err(error) => return Err(error.into()),
    };
    if enforce_custody {
        verify_system_administrators_file_custody(&file)?;
    }
    let observed = observe(&mut file)?;
    if old.is_some_and(|expected| expected == observed) {
        Ok(Slot::Old(file))
    } else if observed == new {
        Ok(Slot::New(file))
    } else {
        Ok(Slot::Other)
    }
}

fn observe_exact(file: &mut File, expected: FileExpectation) -> Result<FileIdentity> {
    let observed = observe(file)?;
    if observed.expectation != expected {
        bail!("file length or SHA-256 does not match the expected immutable bytes");
    }
    Ok(observed)
}

fn observe(file: &mut File) -> Result<FileIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse(&metadata) {
        bail!("backup publish path is not an ordinary non-reparse file");
    }
    if metadata.len() == 0 {
        bail!("backup publish refuses an empty image");
    }
    let id = file_id(file)?;
    let original = file.stream_position()?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut length = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("backup image length overflow"))?;
        hasher.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(original))?;
    if length != metadata.len() || file.metadata()?.len() != metadata.len() || file_id(file)? != id
    {
        bail!("backup image changed while its locked handle was being hashed");
    }
    Ok(FileIdentity {
        id,
        expectation: FileExpectation {
            length,
            sha256: hasher.finalize().into(),
        },
    })
}

fn read_bounded(file: &mut File, maximum: u64) -> Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse(&metadata) || metadata.len() > maximum {
        bail!("backup publish control file is invalid or oversized");
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum || file.metadata()?.len() != metadata.len() {
        bail!("backup publish control file changed while reading");
    }
    Ok(bytes)
}

fn validate_component(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.ends_with(['.', ' '])
        || value.chars().any(|ch| {
            ch <= '\u{1f}' || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
    {
        bail!("backup publish name is not a safe Windows path component");
    }
    let stem = value.split('.').next().unwrap_or(value);
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'))
    {
        bail!("backup publish name is a reserved DOS device");
    }
    Ok(())
}

fn validate_extension(value: &str) -> Result<()> {
    validate_component(value)?;
    if value.contains('.') {
        bail!("backup staged extension must be a single path component suffix");
    }
    if value != "wim" && value != "esd" {
        bail!("backup atomic publication only supports WIM or ESD images");
    }
    Ok(())
}

fn validate_matching_target_extension(target_name: &str, staged_extension: &str) -> Result<()> {
    let target_extension = Path::new(target_name)
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("backup target name omits a WIM/ESD extension"))?;
    if !target_extension.eq_ignore_ascii_case(staged_extension) {
        bail!("backup target and staged image extensions do not match");
    }
    Ok(())
}

fn parse_canonical_u64(value: &str, field: &str) -> Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("backup publish journal {field} is not canonical");
    }
    value.parse().with_context(|| format!("parse {field}"))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

fn decode_hash(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("backup publish journal hash is not canonical lowercase SHA-256");
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => bail!("invalid lowercase hexadecimal digit"),
    }
}

fn reject_existing(path: &Path, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => bail!("backup publish {label} already exists; refusing to replace it"),
    }
}

fn cleanup_path_if_exact(path: &Path, journal: Option<&Journal>) -> Result<()> {
    let mut file = open_file_locked(path)?;
    if let Some(journal) = journal {
        let bytes = read_bounded(&mut file, JOURNAL_MAX_BYTES as u64)?;
        if Journal::parse(&bytes)? != *journal {
            bail!("refusing to delete a journal whose bytes changed");
        }
    }
    delete_on_close(&file)?;
    drop(file);
    Ok(())
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn open_file_locked(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    options.open(path)
}

#[cfg(windows)]
fn open_staged_file_locked(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    options.open(path)
}

#[cfg(windows)]
fn open_file_read_pinned(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ.0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    options.open(path)
}

#[cfg(not(windows))]
fn open_file_locked(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

#[cfg(not(windows))]
fn open_staged_file_locked(path: &Path) -> std::io::Result<File> {
    open_file_locked(path)
}

#[cfg(not(windows))]
fn open_file_read_pinned(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(windows)]
fn open_directory_locked(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup publish directory is not an ordinary non-reparse directory",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_directory_readback(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup publish readback path is not an ordinary non-reparse directory",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_path_identity_readback(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = options.open(path)?;
    if is_reparse(&file.metadata()?) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup publish identity path is a reparse point",
        ));
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_directory_locked(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(not(windows))]
fn open_directory_readback(path: &Path) -> std::io::Result<File> {
    open_directory_locked(path)
}

#[cfg(not(windows))]
fn open_path_identity_readback(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(windows)]
fn file_id(file: &File) -> Result<FileId> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    let index = ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64;
    if index == 0 {
        bail!("filesystem did not provide a stable file id");
    }
    Ok(FileId {
        volume: information.dwVolumeSerialNumber,
        index,
    })
}

#[cfg(not(windows))]
fn file_id(file: &File) -> Result<FileId> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(FileId {
        volume: metadata.dev() as u32,
        index: metadata.ino(),
    })
}

#[cfg(windows)]
fn flush_file(file: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::FlushFileBuffers;
    unsafe { FlushFileBuffers(HANDLE(file.as_raw_handle())) }
        .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))
}

#[cfg(not(windows))]
fn flush_file(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

#[cfg(windows)]
fn rename_handle_no_replace(
    file: &mut File,
    source_path: &Path,
    target_path: &Path,
    moved: &mut bool,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };
    let name = target_path.as_os_str().encode_wide().collect::<Vec<_>>();
    let name_bytes = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| anyhow!("backup publish rename name length overflow"))?;
    let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    // Vista/Windows 7 accept the classic FileRenameInfo class. Keep one trailing UTF-16 NUL in
    // the allocated buffer for compatibility even though FileNameLength excludes it.
    let byte_length = header
        .checked_add(name_bytes)
        .and_then(|length| length.checked_add(std::mem::size_of::<u16>()))
        .ok_or_else(|| anyhow!("backup publish rename buffer overflow"))?;
    let words = byte_length.div_ceil(std::mem::size_of::<u64>());
    let mut buffer = vec![0_u64; words];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    let handle_result = unsafe {
        (*information).Anonymous.ReplaceIfExists = BOOLEAN(0);
        (*information).RootDirectory = HANDLE::default();
        (*information).FileNameLength = u32::try_from(name_bytes)?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr().cast::<u8>(),
            buffer.as_mut_ptr().cast::<u8>().add(header),
            name_bytes,
        );
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileRenameInfo,
            information.cast(),
            u32::try_from(byte_length)?,
        )
    };
    match handle_result {
        Ok(()) => {
            *moved = true;
            Ok(())
        }
        Err(error) if error.code().0 == 0x8007_0020_u32 as i32 => {
            log::warn!(
                "[BACKUP CAS] FileRenameInfo returned ERROR_SHARING_VIOLATION; trying same-volume no-replace MoveFileExW with exact file-ID readback"
            );
            let result = move_file_no_replace_with_identity_readback(
                file,
                source_path,
                target_path,
                moved,
            )
                .with_context(|| {
                    format!(
                        "SetFileInformationByHandle(FileRenameInfo) returned ERROR_SHARING_VIOLATION; compatible MoveFileExW fallback failed (HRESULT 0x{:08x})",
                        error.code().0 as u32
                    )
                });
            if result.is_ok() {
                log::info!(
                    "[BACKUP CAS] MoveFileExW fallback completed with exact source/target file-ID readback"
                );
            }
            result
        }
        Err(error) => Err(anyhow!(
            "SetFileInformationByHandle(FileRenameInfo) failed: {error} (HRESULT 0x{:08x})",
            error.code().0 as u32
        )),
    }
}

#[cfg(not(windows))]
fn rename_handle_no_replace(
    _file: &mut File,
    _source_path: &Path,
    _target_path: &Path,
    _moved: &mut bool,
) -> Result<()> {
    bail!("handle-relative no-replace rename is only supported on Windows")
}

#[cfg(windows)]
fn move_file_no_replace_with_identity_readback(
    file: &mut File,
    source_path: &Path,
    target_path: &Path,
    moved: &mut bool,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let retained_id = file_id(file).context("read retained backup object identity")?;
    let retained_is_dir = file.metadata()?.is_dir();
    let source = open_path_identity_readback(source_path)
        .with_context(|| format!("reopen rename source {}", source_path.display()))?;
    if file_id(&source)? != retained_id || source.metadata()?.is_dir() != retained_is_dir {
        bail!("backup publish rename source pathname no longer identifies its retained object");
    }
    let target_parent_path = target_path
        .parent()
        .ok_or_else(|| anyhow!("backup publish rename target has no parent"))?;
    let target_parent = open_directory_readback(target_parent_path).with_context(|| {
        format!(
            "reopen rename target parent {}",
            target_parent_path.display()
        )
    })?;
    if file_id(&target_parent)?.volume != retained_id.volume {
        bail!("backup publish MoveFileExW fallback would cross filesystem volumes");
    }
    reject_existing(target_path, "rename target")?;

    let mut source_wide = source_path.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut target_wide = target_path.as_os_str().encode_wide().collect::<Vec<_>>();
    if source_wide.contains(&0) || target_wide.contains(&0) {
        bail!("backup publish rename path contains an embedded NUL");
    }
    source_wide.push(0);
    target_wide.push(0);
    // This WinPE filesystem path has been observed reopening the object with sharing requirements
    // that conflict even with a DELETE handle that grants FILE_SHARE_DELETE. Do not weaken the
    // long-lived byte lock: release it only after exact source/parent readback and immediately
    // reacquire the same file ID at the no-replace destination. The caller then rehashes the file.
    drop(source);
    drop(target_parent);
    let placeholder = OpenOptions::new()
        .read(true)
        .open("NUL")
        .context("open inert handle while releasing backup rename custody")?;
    let retained = std::mem::replace(file, placeholder);
    drop(retained);

    let move_result = unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(error) = move_result {
        let primary = anyhow!(
            "MoveFileExW(no replace, write through) failed: {error} (HRESULT 0x{:08x})",
            error.code().0 as u32
        );
        match reopen_locked_object(source_path, retained_is_dir) {
            Ok(restored) if file_id(&restored).ok() == Some(retained_id) => {
                *file = restored;
                if error.code().0 == 0x8007_0020_u32 as i32 && !retained_is_dir {
                    log::warn!(
                        "[BACKUP CAS] MoveFileExW also returned ERROR_SHARING_VIOLATION; trying NTFS no-replace hard-link publication"
                    );
                    return hard_link_file_no_replace_with_identity_readback(
                        file,
                        source_path,
                        target_path,
                        moved,
                    )
                    .with_context(|| format!("{primary:#}; NTFS hard-link fallback failed"));
                }
                return Err(primary);
            }
            Ok(_) => {
                *moved = true;
                return Err(primary).context(
                    "MoveFileExW failed after custody release and the source pathname rebound to another object; preserving PREPARED",
                );
            }
            Err(readback_error) => {
                *moved = true;
                return Err(anyhow!(
                    "{primary:#}; source identity could not be re-established after custody release: {readback_error:#}; preserving PREPARED"
                ));
            }
        }
    }

    // MoveFileExW has already reported success. Set this before every readback so callers treat
    // any later uncertainty as a possible mutation and preserve or exactly roll back PREPARED.
    *moved = true;
    let target = reopen_locked_object(target_path, retained_is_dir)
        .with_context(|| format!("relock moved target {}", target_path.display()))?;
    if file_id(&target)? != retained_id {
        bail!("MoveFileExW target readback does not identify the retained backup object");
    }
    *file = target;
    match std::fs::symlink_metadata(source_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("read back MoveFileExW source pathname"),
        Ok(_) => bail!("MoveFileExW reported success but the source pathname still exists"),
    }
    Ok(())
}

#[cfg(windows)]
fn hard_link_file_no_replace_with_identity_readback(
    file: &mut File,
    source_path: &Path,
    target_path: &Path,
    moved: &mut bool,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::CreateHardLinkW;

    let retained_id = file_id(file).context("read retained backup file identity")?;
    if file.metadata()?.is_dir() {
        bail!("backup hard-link publication only supports ordinary files");
    }
    let source = open_path_identity_readback(source_path)
        .with_context(|| format!("reopen hard-link source {}", source_path.display()))?;
    if source.metadata()?.is_dir() || file_id(&source)? != retained_id {
        bail!("backup hard-link source pathname no longer identifies its retained file");
    }
    let target_parent_path = target_path
        .parent()
        .ok_or_else(|| anyhow!("backup hard-link target has no parent"))?;
    let target_parent = open_directory_readback(target_parent_path)?;
    if file_id(&target_parent)?.volume != retained_id.volume {
        bail!("backup hard-link publication would cross filesystem volumes");
    }
    reject_existing(target_path, "hard-link target")?;

    let mut source_wide = source_path.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut target_wide = target_path.as_os_str().encode_wide().collect::<Vec<_>>();
    if source_wide.contains(&0) || target_wide.contains(&0) {
        bail!("backup hard-link path contains an embedded NUL");
    }
    source_wide.push(0);
    target_wide.push(0);
    drop(source);
    drop(target_parent);
    let placeholder = OpenOptions::new()
        .read(true)
        .open("NUL")
        .context("open inert handle while releasing backup hard-link custody")?;
    let retained = std::mem::replace(file, placeholder);
    drop(retained);

    let link_result = unsafe {
        CreateHardLinkW(
            PCWSTR(target_wide.as_ptr()),
            PCWSTR(source_wide.as_ptr()),
            None,
        )
    };
    if let Err(error) = link_result {
        let primary = anyhow!(
            "CreateHardLinkW(no replace) failed: {error} (HRESULT 0x{:08x})",
            error.code().0 as u32
        );
        match reopen_locked_object(source_path, false) {
            Ok(restored) if file_id(&restored).ok() == Some(retained_id) => {
                *file = restored;
                return Err(primary);
            }
            Ok(_) => {
                *moved = true;
                return Err(primary).context(
                    "CreateHardLinkW failed after custody release and the source pathname rebound; preserving PREPARED",
                );
            }
            Err(readback_error) => {
                *moved = true;
                return Err(anyhow!(
                    "{primary:#}; source identity could not be re-established after hard-link custody release: {readback_error:#}; preserving PREPARED"
                ));
            }
        }
    }

    // The public directory entry now exists. Keep PREPARED authoritative until the exact target
    // file ID is locked, the private link is deleted through an exact handle, and the caller has
    // rehashed the full image. A crash while both links exist is handled explicitly by recovery.
    *moved = true;
    let target = open_file_locked(target_path).with_context(|| {
        format!(
            "lock newly created hard-link target {}",
            target_path.display()
        )
    })?;
    if file_id(&target)? != retained_id {
        bail!("CreateHardLinkW target readback does not identify the retained backup file");
    }
    *file = target;
    let source = open_file_locked(source_path)
        .with_context(|| format!("relock hard-link source {}", source_path.display()))?;
    if file_id(&source)? != retained_id {
        bail!("CreateHardLinkW source readback does not identify the retained backup file");
    }
    delete_on_close(&source).context("delete private staged hard link after publication")?;
    drop(source);
    reject_existing(source_path, "published private staged hard link")?;
    log::info!(
        "[BACKUP CAS] NTFS hard-link fallback completed with exact shared file-ID readback and private-link deletion"
    );
    Ok(())
}

fn rollback_new_publication(
    staged: &mut File,
    target_path: &Path,
    staged_path: &Path,
) -> Result<()> {
    let retained_id = file_id(staged).context("read published image identity during rollback")?;
    match open_path_identity_readback(staged_path) {
        Ok(private_readback) => {
            if private_readback.metadata()?.is_dir() || file_id(&private_readback)? != retained_id {
                bail!("private staged name identifies a competing object during rollback");
            }
            drop(private_readback);
            let placeholder = OpenOptions::new()
                .read(true)
                .open("NUL")
                .context("open inert handle while rolling back public hard link")?;
            let retained = std::mem::replace(staged, placeholder);
            drop(retained);
            let target =
                open_file_locked(target_path).context("lock public image during rollback")?;
            let private =
                open_file_locked(staged_path).context("lock private image during rollback")?;
            *staged = remove_public_hard_link_and_relock_private(
                target,
                private,
                target_path,
                staged_path,
                retained_id,
            )?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut moved = false;
            rename_handle_no_replace(staged, target_path, staged_path, &mut moved)
                .context("move newly published image back into staging during rollback")
        }
        Err(error) => Err(error).context("inspect private staged name during rollback"),
    }
}

fn remove_public_hard_link_and_relock_private(
    public: File,
    private: File,
    public_path: &Path,
    private_path: &Path,
    retained_id: FileId,
) -> Result<File> {
    if file_id(&public)? != retained_id || file_id(&private)? != retained_id {
        bail!("hard-link rollback names no longer identify the same retained file");
    }
    // Basic FileDispositionInfo completes after the file's other handles close. Release the
    // private-name handle before marking/deleting the public link, then immediately reopen the
    // still-existing private name and prove its original file ID.
    drop(private);
    delete_on_close(&public).context("remove interrupted public hard link")?;
    drop(public);
    reject_existing(public_path, "rolled-back public hard link")?;
    let private = open_file_locked(private_path).context("relock private staged hard link")?;
    if file_id(&private)? != retained_id {
        bail!("private staged hard link changed while the public link was removed");
    }
    Ok(private)
}

#[cfg(windows)]
fn reopen_locked_object(path: &Path, is_directory: bool) -> Result<File> {
    let file = if is_directory {
        open_directory_locked(path)?
    } else {
        open_file_locked(path)?
    };
    if file.metadata()?.is_dir() != is_directory {
        bail!("backup publish pathname changed object type while reacquiring custody");
    }
    Ok(file)
}

#[cfg(windows)]
fn delete_on_close(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };
    let information = FILE_DISPOSITION_INFO {
        DeleteFile: BOOLEAN(1),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&information as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    Ok(())
}

#[cfg(not(windows))]
fn delete_on_close(_file: &File) -> Result<()> {
    bail!("handle deletion is only supported on Windows")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TestTree {
        root: PathBuf,
        session: SecurePublishSession,
        session_id: SessionId,
    }

    impl TestTree {
        fn new() -> Self {
            let session_id = SessionId::parse("0123456789abcdef0123456789abcdef").unwrap();
            let root = std::env::temp_dir().join(format!(
                "lr-backup-publish-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            let session_path = root.join(format!(
                ".LetRecovery-BackupPublish-{}",
                session_id.as_str()
            ));
            std::fs::create_dir(&session_path).unwrap();
            let pins = pin_existing_parent_directory_ancestors(&root).unwrap();
            let directory = open_directory_locked(&session_path).unwrap();
            let parent = open_directory_locked(&root).unwrap();
            let session = SecurePublishSession {
                directory_identity: file_id(&directory).unwrap(),
                parent_identity: file_id(&parent).unwrap(),
                path: session_path,
                session_id: session_id.clone(),
                _pins: pins,
            };
            drop(directory);
            drop(parent);
            Self {
                root,
                session,
                session_id,
            }
        }

        fn write(&self, path: &Path, bytes: &[u8]) {
            let mut file = File::create(path).unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            // All transaction handles are gone before TestTree::drop. Tests only use ordinary
            // throwaway files and never WIM/DISM or a real backup destination.
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn publish_create_through_secure_session(parent: &Path) {
        let session_id = SessionId::parse("fedcba9876543210fedcba9876543210").unwrap();
        let mut session = SecurePublishSession::create(parent, &session_id).unwrap();
        let (_staged, mut writer) = session.create_staged_file("wim").unwrap();
        writer.write_all(b"created").unwrap();
        writer.flush().unwrap();
        flush_file(&writer).unwrap();
        drop(writer);
        let completed = session.inspect_staged_file("wim").unwrap();
        publish_create(&mut session, "backup.wim", "wim", completed).unwrap();
        session.remove_empty().unwrap();
    }

    #[test]
    fn secure_session_create_publishes_with_the_transferred_directory_handle() {
        if !crate::scoped_temp_file::test_token_is_elevated_administrator().unwrap() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "lr-backup-secure-create-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        publish_create_through_secure_session(&root);
        assert_eq!(std::fs::read(root.join("backup.wim")).unwrap(), b"created");
        std::fs::remove_file(root.join("backup.wim")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn secure_session_create_accepts_a_volume_guid_namespace_parent() {
        if !crate::scoped_temp_file::test_token_is_elevated_administrator().unwrap() {
            return;
        }
        use std::ffi::{OsStr, OsString};
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;

        let root = std::env::temp_dir().join(format!(
            "lr-backup-volume-guid-create-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let text = root.to_string_lossy();
        assert!(text.len() >= 3 && text.as_bytes()[1] == b':' && text.as_bytes()[2] == b'\\');
        let drive_root = &text[..3];
        let mount = OsStr::new(drive_root)
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut volume = [0_u16; 128];
        unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount.as_ptr()), &mut volume) }.unwrap();
        let length = volume.iter().position(|value| *value == 0).unwrap();
        let guid_root = PathBuf::from(OsString::from_wide(&volume[..length]));
        let guid_parent = guid_root.join(&text[3..]);

        publish_create_through_secure_session(&guid_parent);
        assert_eq!(std::fs::read(root.join("backup.wim")).unwrap(), b"created");
        std::fs::remove_file(root.join("backup.wim")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    #[ignore = "explicit elevated large-WIM filesystem integration test"]
    fn secure_session_publishes_after_wimlib_verification() {
        if !crate::scoped_temp_file::test_token_is_elevated_administrator().unwrap() {
            return;
        }
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(r"pkg\bin\pe\LetRecovery_PE.wim");
        assert!(fixture.is_file());
        let root = std::env::temp_dir().join(format!(
            "lr-backup-wimlib-publish-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let session_id = SessionId::parse("00112233445566778899aabbccddeeff").unwrap();
        let mut session = SecurePublishSession::create(&root, &session_id).unwrap();
        let (staged, mut writer) = session.create_staged_file("wim").unwrap();
        let mut source = File::open(&fixture).unwrap();
        std::io::copy(&mut source, &mut writer).unwrap();
        writer.flush().unwrap();
        flush_file(&writer).unwrap();
        drop(source);
        drop(writer);

        let catalog = crate::wimlib::read_verified_backup_catalog(&staged).unwrap();
        assert!(!catalog.images().is_empty());
        let completed = session.inspect_staged_file("wim").unwrap();
        publish_create(&mut session, "backup.wim", "wim", completed).unwrap();
        session.remove_empty().unwrap();
        assert!(root.join("backup.wim").is_file());
        std::fs::remove_file(root.join("backup.wim")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    #[ignore = "explicit elevated wimlib capture and durable-publication integration test"]
    fn secure_session_publishes_a_fresh_wimlib_capture() {
        if !crate::scoped_temp_file::test_token_is_elevated_administrator().unwrap() {
            return;
        }
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let source = std::env::temp_dir().join(format!(
            "lr-backup-wimlib-capture-source-{}-{nonce}",
            std::process::id()
        ));
        let target = std::env::temp_dir().join(format!(
            "lr-backup-wimlib-capture-target-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(source.join("payload.txt"), b"captured payload").unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(r"pkg\bin\pe\LetRecovery_PE.wim");
        assert!(fixture.is_file());
        std::fs::copy(&fixture, source.join("large-payload.wim")).unwrap();

        let session_id = SessionId::parse("11223344556677889900aabbccddeeff").unwrap();
        let mut session = SecurePublishSession::create(&target, &session_id).unwrap();
        let staged = session.staged_path("wim");
        {
            let manager = crate::wimlib::WimlibManager::new().unwrap();
            manager
                .capture_image(
                    &source.to_string_lossy(),
                    &staged.to_string_lossy(),
                    "Captured",
                    "Captured image",
                    2,
                    None,
                )
                .unwrap();
        }
        let catalog = crate::wimlib::read_verified_backup_catalog(&staged).unwrap();
        assert_eq!(catalog.images().len(), 1);
        crate::scoped_temp_file::restrict_to_system_and_administrators(&staged).unwrap();
        let completed = session.inspect_staged_file("wim").unwrap();
        publish_create(&mut session, "backup.wim", "wim", completed).unwrap();
        session.remove_empty().unwrap();
        assert!(target.join("backup.wim").is_file());

        std::fs::remove_file(target.join("backup.wim")).unwrap();
        std::fs::remove_dir(target).unwrap();
        std::fs::remove_file(source.join("payload.txt")).unwrap();
        std::fs::remove_file(source.join("large-payload.wim")).unwrap();
        std::fs::remove_dir(source).unwrap();
    }

    struct FailAt(FaultPoint);
    impl FaultInjector for FailAt {
        fn hit(&self, point: FaultPoint, _session: &SecurePublishSession) -> Result<()> {
            if self.0 == point {
                bail!("simulated process crash")
            }
            Ok(())
        }
    }

    struct ClaimTargetAfterOldMoved;
    impl FaultInjector for ClaimTargetAfterOldMoved {
        fn hit(&self, point: FaultPoint, session: &SecurePublishSession) -> Result<()> {
            if point == FaultPoint::OldMoved {
                std::fs::write(
                    session.path.parent().unwrap().join("backup.wim"),
                    b"competitor",
                )?;
            }
            Ok(())
        }
    }

    fn create_journal_for_recovery(
        tree: &TestTree,
        mode: PublishMode,
        old: Option<FileIdentity>,
        new: FileIdentity,
    ) {
        let journal = Journal {
            mode,
            session_id: tree.session_id.as_str().to_owned(),
            target_name: "backup.wim".to_owned(),
            staged_extension: "wim".to_owned(),
            old,
            new,
        };
        tree.write(&tree.session.journal_path(), &journal.encode());
    }

    fn identity(path: &Path) -> FileIdentity {
        let mut file = open_file_locked(path).unwrap();
        observe(&mut file).unwrap()
    }

    #[test]
    fn journal_is_canonical_bounded_and_checksum_detects_tampering() {
        let old = FileIdentity {
            id: FileId {
                volume: 7,
                index: 9,
            },
            expectation: FileExpectation::from_bytes(b"old"),
        };
        let new = FileIdentity {
            id: FileId {
                volume: 7,
                index: 10,
            },
            expectation: FileExpectation::from_bytes(b"new"),
        };
        let journal = Journal {
            mode: PublishMode::Replace,
            session_id: "0123456789abcdef0123456789abcdef".to_owned(),
            target_name: "backup.wim".to_owned(),
            staged_extension: "wim".to_owned(),
            old: Some(old),
            new,
        };
        let encoded = journal.encode();
        assert!(encoded.len() < JOURNAL_MAX_BYTES);
        assert_eq!(Journal::parse(&encoded).unwrap(), journal);
        let mut tampered = encoded;
        let position = tampered.iter().position(|byte| *byte == b'9').unwrap();
        tampered[position] = b'8';
        assert!(Journal::parse(&tampered).is_err());
    }

    #[test]
    fn replace_uses_no_replace_handle_renames_and_commits() {
        let mut tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"old-image");
        tree.write(&staged, b"new-image");
        let old = FileExpectation::from_bytes(b"old-image");
        let new = FileExpectation::from_bytes(b"new-image");
        publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Replace,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: Some(old),
                expected_new: new,
                enforce_custody: false,
            },
            None,
            &NoFault,
        )
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-image");
        assert!(!tree.session.previous_path().exists());
        assert!(!tree.session.journal_path().exists());
    }

    #[test]
    fn injected_failures_after_each_rename_are_deterministically_rolled_back() {
        for point in [FaultPoint::OldMoved, FaultPoint::NewPublished] {
            let mut tree = TestTree::new();
            let target = tree.root.join("backup.wim");
            let staged = tree.session.staged_path("wim");
            tree.write(&target, b"old");
            tree.write(&staged, b"new");
            let error = publish_impl(
                &mut tree.session,
                PublishSpec {
                    mode: PublishMode::Replace,
                    target_name: "backup.wim",
                    staged_extension: "wim",
                    expected_old: Some(FileExpectation::from_bytes(b"old")),
                    expected_new: FileExpectation::from_bytes(b"new"),
                    enforce_custody: false,
                },
                None,
                &FailAt(point),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("rolled back"),
                "unexpected publication error: {error:#}"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            assert_eq!(std::fs::read(&staged).unwrap(), b"new");
            assert!(!tree.session.previous_path().exists());
            assert!(!tree.session.journal_path().exists());
        }
    }

    #[test]
    fn prepared_crash_is_recovered_from_actual_slots_not_journal_state() {
        let mut tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"old");
        tree.write(&staged, b"new");
        publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Append,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: Some(FileExpectation::from_bytes(b"old")),
                expected_new: FileExpectation::from_bytes(b"new"),
                enforce_custody: false,
            },
            None,
            &FailAt(FaultPoint::PreparedPersisted),
        )
        .unwrap_err();
        assert!(tree.session.journal_path().exists());
        assert_eq!(
            recover_existing_impl(
                &tree.session,
                PublishMode::Append,
                "backup.wim",
                "wim",
                false,
            )
            .unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(target).unwrap(), b"old");
        assert!(!staged.exists());
    }

    #[test]
    fn recovery_rolls_back_after_old_move_using_actual_hashes_and_ids() {
        let tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"old");
        tree.write(&staged, b"new");
        let old = identity(&target);
        let new = identity(&staged);
        create_journal_for_recovery(&tree, PublishMode::Append, Some(old), new);
        let mut old_handle = open_file_locked(&target).unwrap();
        let mut moved = false;
        rename_handle_no_replace(
            &mut old_handle,
            &target,
            &tree.session.previous_path(),
            &mut moved,
        )
        .unwrap();
        drop(old_handle);
        assert_eq!(
            recover_impl(&tree.session, "backup.wim", "wim", None, false).unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(target).unwrap(), b"old");
        assert!(!staged.exists());
    }

    #[test]
    fn competing_target_is_never_replaced_and_previous_is_preserved() {
        let mut tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"old");
        tree.write(&staged, b"new");
        let error = publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Replace,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: Some(FileExpectation::from_bytes(b"old")),
                expected_new: FileExpectation::from_bytes(b"new"),
                enforce_custody: false,
            },
            None,
            &ClaimTargetAfterOldMoved,
        )
        .unwrap_err();
        assert!(error.to_string().contains("rollback could not be proven"));
        assert_eq!(std::fs::read(target).unwrap(), b"competitor");
        assert_eq!(std::fs::read(tree.session.previous_path()).unwrap(), b"old");
        assert!(tree.session.journal_path().exists());
    }

    #[test]
    fn create_recovery_commits_only_the_exact_staged_file() {
        let tree = TestTree::new();
        let staged = tree.session.staged_path("wim");
        let target = tree.root.join("backup.wim");
        tree.write(&staged, b"created");
        let new = identity(&staged);
        create_journal_for_recovery(&tree, PublishMode::Create, None, new);
        let mut staged_handle = open_file_locked(&staged).unwrap();
        let mut moved = false;
        rename_handle_no_replace(&mut staged_handle, &staged, &target, &mut moved).unwrap();
        drop(staged_handle);
        assert_eq!(
            recover_impl(&tree.session, "backup.wim", "wim", None, false).unwrap(),
            RecoveryOutcome::Committed
        );
        assert_eq!(std::fs::read(target).unwrap(), b"created");
    }

    #[test]
    fn create_recovery_rejects_a_replace_or_append_journal() {
        let tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"old");
        tree.write(&staged, b"new");
        create_journal_for_recovery(
            &tree,
            PublishMode::Append,
            Some(identity(&target)),
            identity(&staged),
        );

        let error = recover_impl(
            &tree.session,
            "backup.wim",
            "wim",
            Some(PublishMode::Create),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("mode does not match"));
        assert!(tree.session.journal_path().exists());
        assert_eq!(std::fs::read(target).unwrap(), b"old");
        assert_eq!(std::fs::read(staged).unwrap(), b"new");
    }

    #[test]
    fn create_commit_receipt_closes_the_post_journal_crash_window() {
        let mut tree = TestTree::new();
        let staged = tree.session.staged_path("wim");
        let target = tree.root.join("backup.wim");
        tree.write(&staged, b"verified-completed-image");
        publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Create,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: None,
                expected_new: FileExpectation::from_bytes(b"verified-completed-image"),
                enforce_custody: false,
            },
            None,
            &NoFault,
        )
        .unwrap();
        assert!(tree.session.committed_receipt().unwrap().is_some());
        assert!(!tree.session.journal_path().exists());
        assert_eq!(
            recover_create_impl(&tree.session, "backup.wim", "wim", false).unwrap(),
            RecoveryOutcome::Committed
        );
        assert_eq!(std::fs::read(target).unwrap(), b"verified-completed-image");
    }

    #[test]
    fn existing_preparation_blocks_writers_and_survives_supported_path_rename() {
        let tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        tree.write(&target, b"old-image");
        let mut preparation = tree
            .session
            .prepare_existing_copy_impl("backup.wim", "wim", false)
            .unwrap();

        assert_eq!(
            std::fs::read(tree.session.staged_path("wim")).unwrap(),
            b"old-image"
        );
        assert!(OpenOptions::new().write(true).open(&target).is_err());
        let moved = tree.root.join("moved.wim");
        std::fs::rename(&target, &moved).unwrap();
        assert_eq!(
            observe_exact(&mut preparation.old, preparation.identity.expectation).unwrap(),
            preparation.identity
        );
        std::fs::rename(&moved, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old-image");
        drop(preparation);
        assert!(OpenOptions::new().write(true).open(&target).is_ok());
    }

    #[test]
    fn movefile_fallback_preserves_identity_and_never_replaces() {
        let tree = TestTree::new();
        let source = tree.root.join("fallback-source.wim");
        let target = tree.root.join("fallback-target.wim");
        tree.write(&source, b"fallback-image");
        let mut source_handle = open_file_locked(&source).unwrap();
        let retained = file_id(&source_handle).unwrap();
        let mut moved = false;
        move_file_no_replace_with_identity_readback(
            &mut source_handle,
            &source,
            &target,
            &mut moved,
        )
        .unwrap();
        assert!(moved);
        assert!(!source.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"fallback-image");
        assert_eq!(file_id(&source_handle).unwrap(), retained);
        assert!(OpenOptions::new().write(true).open(&target).is_err());
        assert_eq!(
            file_id(&open_path_identity_readback(&target).unwrap()).unwrap(),
            retained
        );

        let competing_source = tree.root.join("competing-source.wim");
        tree.write(&competing_source, b"must-stay-source");
        let mut competing_handle = open_file_locked(&competing_source).unwrap();
        let mut competing_moved = false;
        let error = move_file_no_replace_with_identity_readback(
            &mut competing_handle,
            &competing_source,
            &target,
            &mut competing_moved,
        )
        .unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert!(!competing_moved);
        assert_eq!(
            std::fs::read(competing_source).unwrap(),
            b"must-stay-source"
        );
        assert_eq!(std::fs::read(target).unwrap(), b"fallback-image");
    }

    #[test]
    fn movefile_fallback_moves_from_private_session_into_public_parent() {
        let tree = TestTree::new();
        let source = tree.session.staged_path("wim");
        let target = tree.root.join("fallback-published.wim");
        tree.write(&source, b"cross-directory-image");
        let mut source_handle = open_file_locked(&source).unwrap();
        let retained = file_id(&source_handle).unwrap();
        let mut moved = false;

        move_file_no_replace_with_identity_readback(
            &mut source_handle,
            &source,
            &target,
            &mut moved,
        )
        .unwrap();

        assert!(moved);
        assert!(!source.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"cross-directory-image");
        assert_eq!(file_id(&source_handle).unwrap(), retained);
        assert!(OpenOptions::new().write(true).open(&target).is_err());
    }

    #[test]
    #[cfg(windows)]
    fn hard_link_fallback_publishes_exact_file_and_never_replaces() {
        let tree = TestTree::new();
        let source = tree.session.staged_path("wim");
        let target = tree.root.join("hard-link-published.wim");
        tree.write(&source, b"hard-link-image");
        let mut source_handle = open_file_locked(&source).unwrap();
        let retained = file_id(&source_handle).unwrap();
        let mut moved = false;

        hard_link_file_no_replace_with_identity_readback(
            &mut source_handle,
            &source,
            &target,
            &mut moved,
        )
        .unwrap();

        assert!(moved);
        assert!(!source.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"hard-link-image");
        assert_eq!(file_id(&source_handle).unwrap(), retained);
        assert!(OpenOptions::new().write(true).open(&target).is_err());

        let competing_source = tree.session.path().join("competing.wim");
        tree.write(&competing_source, b"must-remain-private");
        let mut competing_handle = open_file_locked(&competing_source).unwrap();
        let mut competing_moved = false;
        assert!(hard_link_file_no_replace_with_identity_readback(
            &mut competing_handle,
            &competing_source,
            &target,
            &mut competing_moved,
        )
        .is_err());
        assert!(!competing_moved);
        assert_eq!(
            std::fs::read(competing_source).unwrap(),
            b"must-remain-private"
        );
        assert_eq!(std::fs::read(target).unwrap(), b"hard-link-image");
    }

    #[test]
    #[cfg(windows)]
    fn rollback_removes_only_public_link_when_private_link_still_exists() {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::CreateHardLinkW;

        let tree = TestTree::new();
        let staged_path = tree.session.staged_path("wim");
        let target_path = tree.root.join("hard-link-interrupted.wim");
        tree.write(&staged_path, b"interrupted-hard-link-image");
        let mut staged = open_file_locked(&staged_path).unwrap();
        let retained = file_id(&staged).unwrap();
        drop(staged);
        let mut staged_wide = staged_path.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut target_wide = target_path.as_os_str().encode_wide().collect::<Vec<_>>();
        staged_wide.push(0);
        target_wide.push(0);
        unsafe {
            CreateHardLinkW(
                PCWSTR(target_wide.as_ptr()),
                PCWSTR(staged_wide.as_ptr()),
                None,
            )
        }
        .unwrap();
        staged = open_file_locked(&target_path).unwrap();

        rollback_new_publication(&mut staged, &target_path, &staged_path).unwrap();

        assert!(!target_path.exists());
        assert_eq!(
            std::fs::read(&staged_path).unwrap(),
            b"interrupted-hard-link-image"
        );
        assert_eq!(
            file_id(&open_file_locked(&staged_path).unwrap()).unwrap(),
            retained
        );
    }

    #[test]
    #[cfg(windows)]
    fn recovery_rolls_back_interrupted_duplicate_hard_links_by_exact_id() {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::CreateHardLinkW;

        let tree = TestTree::new();
        let target_name = "hard-link-recovery.wim";
        let staged_path = tree.session.staged_path("wim");
        let target_path = tree.root.join(target_name);
        tree.write(&staged_path, b"recoverable-hard-link-image");
        let mut staged = open_file_locked(&staged_path).unwrap();
        let new = observe(&mut staged).unwrap();
        drop(staged);
        let mut staged_wide = staged_path.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut target_wide = target_path.as_os_str().encode_wide().collect::<Vec<_>>();
        staged_wide.push(0);
        target_wide.push(0);
        unsafe {
            CreateHardLinkW(
                PCWSTR(target_wide.as_ptr()),
                PCWSTR(staged_wide.as_ptr()),
                None,
            )
        }
        .unwrap();
        let journal = Journal {
            mode: PublishMode::Create,
            session_id: tree.session_id.as_str().to_owned(),
            target_name: target_name.to_owned(),
            staged_extension: "wim".to_owned(),
            old: None,
            new,
        };
        tree.write(&tree.session.journal_path(), &journal.encode());

        assert_eq!(
            recover_create_impl(&tree.session, target_name, "wim", false).unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert!(!target_path.exists());
        assert!(!staged_path.exists());
        assert!(!tree.session.journal_path().exists());
    }

    #[test]
    fn prepared_staged_inspection_is_bound_to_the_copied_file_id_and_bytes() {
        let tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        tree.write(&target, b"old-image");
        let mut preparation = tree
            .session
            .prepare_existing_copy_impl("backup.wim", "wim", false)
            .unwrap();

        let observed = tree
            .session
            .inspect_prepared_staged_impl(&mut preparation, |path| Ok(std::fs::read(path)?), false)
            .unwrap();
        assert_eq!(observed, b"old-image");

        std::fs::write(tree.session.staged_path("wim"), b"changed").unwrap();
        let error = tree
            .session
            .inspect_prepared_staged_impl(&mut preparation, |_| Ok(()), false)
            .unwrap_err();
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn move_only_preparation_publishes_append_and_preserves_old_until_cas() {
        let mut tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        tree.write(&target, b"old-image");
        let preparation = tree
            .session
            .prepare_existing_copy_impl("backup.wim", "wim", false)
            .unwrap();
        let staged = tree.session.staged_path("wim");
        OpenOptions::new()
            .append(true)
            .open(&staged)
            .unwrap()
            .write_all(b"+appended")
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old-image");

        let expected_old = preparation.identity.expectation;
        let prepared_old = Some((preparation.old, preparation.identity));
        publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Append,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: Some(expected_old),
                expected_new: FileExpectation::from_bytes(b"old-image+appended"),
                enforce_custody: false,
            },
            prepared_old,
            &NoFault,
        )
        .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"old-image+appended");
        assert_eq!(
            tree.session.committed_receipt().unwrap().unwrap().0,
            PublishMode::Append
        );
        assert!(recover_existing_impl(
            &tree.session,
            PublishMode::Replace,
            "backup.wim",
            "wim",
            false,
        )
        .is_err());
        assert_eq!(
            recover_existing_impl(
                &tree.session,
                PublishMode::Append,
                "backup.wim",
                "wim",
                false,
            )
            .unwrap(),
            RecoveryOutcome::Committed
        );
    }

    #[test]
    fn replacement_reset_deletes_only_the_bound_copy_and_keeps_old_locked() {
        let mut tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        tree.write(&target, b"old-image");
        let mut preparation = tree
            .session
            .prepare_existing_copy_impl("backup.wim", "wim", false)
            .unwrap();
        tree.session
            .discard_copied_staged_for_replace_impl(&mut preparation, false)
            .unwrap();
        assert!(!tree.session.staged_path("wim").exists());
        assert!(OpenOptions::new().write(true).open(&target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old-image");

        let staged = tree.session.staged_path("wim");
        tree.write(&staged, b"replacement-image");
        let expected_old = preparation.identity.expectation;
        let prepared_old = Some((preparation.old, preparation.identity));
        publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Replace,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: Some(expected_old),
                expected_new: FileExpectation::from_bytes(b"replacement-image"),
                enforce_custody: false,
            },
            prepared_old,
            &NoFault,
        )
        .unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"replacement-image");
    }

    #[test]
    fn existing_unprepared_recovery_removes_only_staging_and_preserves_target() {
        let tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"old-image");
        tree.write(&staged, b"partial-copy");

        assert_eq!(
            recover_existing_impl(
                &tree.session,
                PublishMode::Replace,
                "backup.wim",
                "wim",
                false,
            )
            .unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(target).unwrap(), b"old-image");
        assert!(!staged.exists());
    }

    #[test]
    fn session_path_rebinding_is_detected_from_the_retained_directory_id() {
        let tree = TestTree::new();
        let original = tree.session.path.clone();
        let moved = tree.root.join("moved-session");
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        let error = tree.session.verify_pins().unwrap_err();
        assert!(error
            .to_string()
            .contains("no longer identifies its retained directory"));
        std::fs::remove_dir(&original).unwrap();
        std::fs::rename(&moved, &original).unwrap();
        tree.session.verify_pins().unwrap();
    }

    #[test]
    fn expectation_and_path_validation_fail_closed() {
        assert!(validate_component("..\\escape.wim").is_err());
        assert!(validate_component("CON.wim").is_err());
        assert!(validate_extension("wim.tmp").is_err());
        assert!(validate_matching_target_extension("backup.esd", "wim").is_err());
        assert!(parse_canonical_u64("01", "value").is_err());
        assert!(decode_hash(&"A".repeat(64)).is_err());
        let _ = ClaimTargetAfterOldMoved;
    }

    #[test]
    fn existing_and_staged_hardlinks_are_rejected_as_one_file_object() {
        let mut tree = TestTree::new();
        let target = tree.root.join("backup.wim");
        let staged = tree.session.staged_path("wim");
        tree.write(&target, b"same-bytes");
        std::fs::hard_link(&target, &staged).unwrap();
        let expectation = FileExpectation::from_bytes(b"same-bytes");
        let error = publish_impl(
            &mut tree.session,
            PublishSpec {
                mode: PublishMode::Replace,
                target_name: "backup.wim",
                staged_extension: "wim",
                expected_old: Some(expectation),
                expected_new: expectation,
                enforce_custody: false,
            },
            None,
            &NoFault,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("lock existing backup"));
        assert_eq!(std::fs::read(target).unwrap(), b"same-bytes");
        assert!(!tree.session.journal_path().exists());
    }
}
