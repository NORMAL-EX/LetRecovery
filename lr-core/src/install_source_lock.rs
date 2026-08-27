//! Immutable install-image handle set held from verification through apply.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct LockedFile {
    path: PathBuf,
    file: File,
    length: u64,
    sha256: String,
}

/// Immutable identity of one file held by an install-source guard.
///
/// The path, length and digest are all derived from the same deny-write/delete handle that
/// remains owned by the guard. Callers must keep that guard alive while publishing a manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedSourceArtifactIdentity {
    pub path: PathBuf,
    pub length_bytes: u64,
    pub sha256: [u8; 32],
}

/// Locks every file consumed by a WIM/ESD/SWM or GHO/GHS apply operation.
///
/// Windows sharing is restricted to readers, so a verified file cannot be written,
/// renamed or deleted before the guard is dropped. `verify_unchanged` additionally
/// re-enumerates split spans and hashes the same open handles.
#[derive(Debug)]
pub struct LockedInstallSourceSet {
    selected: PathBuf,
    files: Vec<LockedFile>,
    pinned_parents: Vec<crate::scoped_temp_file::PinnedDirectoryAncestors>,
    _stage_dir: Option<crate::scoped_temp_file::ScopedTempDir>,
}

#[derive(Debug)]
struct LockedTreeFile {
    path: PathBuf,
    file: File,
    length: u64,
    sha256: String,
}

/// Exact, open-handle manifest for an XP/2003 I386 or AMD64 source tree.
#[derive(Debug)]
pub struct LockedInstallTree {
    selected: PathBuf,
    roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    files: Vec<LockedTreeFile>,
    pinned_parents: Vec<crate::scoped_temp_file::PinnedDirectoryAncestors>,
}

/// One ordinary, non-reparse public artifact held deny-write/delete while its authenticated
/// manifest is published.
#[derive(Debug)]
pub struct LockedPlainArtifact {
    identity: LockedSourceArtifactIdentity,
    file: File,
    pinned_parents: crate::scoped_temp_file::PinnedDirectoryAncestors,
}

const MAX_LOCKED_TREE_FILES: usize = 65_536;

impl LockedPlainArtifact {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        Self::acquire_with_progress(path, |_| {})
    }

    /// Open one manifest artifact once and calculate its authenticated digest while reporting
    /// bytes read from that same deny-write/delete handle.  WinPE uses this to expose useful
    /// progress before any target write instead of appearing frozen while a multi-gigabyte image
    /// is read.
    pub fn acquire_with_progress(
        path: &Path,
        on_progress: impl FnMut(u64),
    ) -> Result<Self, String> {
        validate_non_reparse_path(path)?;
        let canonical = std::fs::canonicalize(path)
            .map_err(|error| format!("canonicalize public artifact: {error}"))?;
        let parent = canonical
            .parent()
            .ok_or_else(|| "public artifact has no parent".to_owned())?;
        let pinned_parents = crate::scoped_temp_file::pin_existing_directory_ancestors(parent)
            .map_err(|error| format!("pin public artifact ancestors: {error}"))?;
        let file = open_locked(&canonical)
            .map_err(|error| format!("lock public artifact {}: {error}", canonical.display()))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err("public artifact must be a non-empty ordinary file".to_owned());
        }
        let mut reader = file
            .try_clone()
            .map_err(|error| format!("clone public artifact handle: {error}"))?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("rewind public artifact handle: {error}"))?;
        let sha256 = crate::hash::sha256_reader(&mut reader, on_progress)
            .map_err(|error| error.to_string())?;
        let identity = LockedSourceArtifactIdentity {
            path: canonical,
            length_bytes: metadata.len(),
            sha256: crate::install_handoff::decode_hex_array::<32>(
                &sha256,
                "locked public artifact SHA-256",
            )
            .map_err(|error| error.to_string())?,
        };
        Ok(Self {
            identity,
            file,
            pinned_parents,
        })
    }

    /// Recheck only the pathname/ancestor binding and immutable handle length.
    ///
    /// `acquire_with_progress` already authenticated the complete contents against LRHM3 while
    /// the handle was opened with read-only sharing. Re-hashing the same multi-gigabyte image at
    /// every preflight stage adds minutes of I/O without closing a new mutation window: another
    /// writer/deleter cannot open the file until this guard is dropped.
    pub fn verify_binding_unchanged(&self) -> Result<(), String> {
        self.pinned_parents
            .verify_unchanged()
            .map_err(|error| format!("public artifact ancestor changed: {error}"))?;
        let current = std::fs::canonicalize(&self.identity.path)
            .map_err(|error| format!("rebind public artifact: {error}"))?;
        let metadata = self.file.metadata().map_err(|error| error.to_string())?;
        if current != self.identity.path || metadata.len() != self.identity.length_bytes {
            return Err(format!(
                "public artifact binding changed after capture: {}",
                self.identity.path.display()
            ));
        }
        Ok(())
    }

    pub fn identity(&self) -> &LockedSourceArtifactIdentity {
        &self.identity
    }

    pub fn verify_unchanged(&self) -> Result<(), String> {
        self.verify_binding_unchanged()?;
        let sha256 = hash_handle(&self.file).map_err(|error| error.to_string())?;
        if crate::install_handoff::decode_hex_array::<32>(&sha256, "locked public artifact SHA-256")
            .map_err(|error| error.to_string())?
            != self.identity.sha256
        {
            return Err(format!(
                "public artifact changed after capture: {}",
                self.identity.path.display()
            ));
        }
        Ok(())
    }

    /// Copy bytes from the same deny-write/delete source handle into a caller-owned protected
    /// destination handle.  This is used by WinPE to materialize an exact-set directory snapshot
    /// without reopening attacker-writable public paths after manifest verification.
    pub fn copy_to_verified_writer(&self, destination: &mut File) -> Result<(), String> {
        use std::io::{Seek, SeekFrom};

        self.verify_unchanged()?;
        let mut source = self
            .file
            .try_clone()
            .map_err(|error| format!("clone locked artifact handle: {error}"))?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("rewind locked artifact: {error}"))?;
        destination
            .set_len(0)
            .map_err(|error| format!("truncate protected artifact snapshot: {error}"))?;
        destination
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("rewind protected artifact snapshot: {error}"))?;
        let copied = std::io::copy(&mut source, destination)
            .map_err(|error| format!("copy locked artifact into protected snapshot: {error}"))?;
        destination
            .sync_all()
            .map_err(|error| format!("flush protected artifact snapshot: {error}"))?;
        if copied != self.identity.length_bytes {
            return Err("protected artifact snapshot length does not match manifest".to_owned());
        }
        let digest = hash_handle(destination).map_err(|error| error.to_string())?;
        if crate::install_handoff::decode_hex_array::<32>(
            &digest,
            "protected artifact snapshot SHA-256",
        )
        .map_err(|error| error.to_string())?
            != self.identity.sha256
        {
            return Err("protected artifact snapshot SHA-256 does not match manifest".to_owned());
        }
        self.verify_unchanged()
    }
}

fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn validate_non_reparse_path(path: &Path) -> Result<(), String> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(ancestor)
            .map_err(|error| format!("inspect source ancestor {}: {error}", ancestor.display()))?;
        if metadata_is_reparse(&metadata) {
            return Err(format!(
                "install source traverses a reparse point: {}",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))?;
    value.get(prefix.len()..)
}

/// Enumerate the exact ordered span set an image engine will consume.
pub fn enumerate_install_image_set(source: &Path) -> Result<Vec<PathBuf>, String> {
    let source = std::fs::canonicalize(source)
        .map_err(|error| format!("canonicalize install source: {error}"))?;
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "install source has no Unicode file name".to_string())?;
    if extension_is(&source, "ghs") {
        return Err("select the primary .gho volume instead of a .ghs span".into());
    }
    if !extension_is(&source, "swm") && !extension_is(&source, "gho") {
        return Ok(vec![source]);
    }
    let parent = source
        .parent()
        .ok_or_else(|| "split image has no parent directory".to_string())?;
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "split image has no Unicode stem".to_string())?;

    if extension_is(&source, "swm") {
        let trimmed = stem.trim_end_matches(|value: char| value.is_ascii_digit());
        if trimmed.len() != stem.len()
            && !trimmed.is_empty()
            && parent.join(format!("{trimmed}.swm")).is_file()
        {
            return Err("select the primary SWM volume".into());
        }
        let mut indexed = BTreeMap::new();
        for entry in std::fs::read_dir(parent).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if !extension_is(&path, "swm") {
                continue;
            }
            let Some(candidate) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let index = if candidate.eq_ignore_ascii_case(stem) {
                Some(1)
            } else {
                strip_prefix_ascii_case(candidate, stem)
                    .filter(|suffix| !suffix.is_empty())
                    .filter(|suffix| suffix.bytes().all(|byte| byte.is_ascii_digit()))
                    .and_then(|suffix| suffix.parse::<usize>().ok().map(|index| (suffix, index)))
                    .filter(|(suffix, index)| *index >= 2 && *suffix == index.to_string())
                    .map(|(_, index)| index)
            };
            if let Some(index) = index {
                if indexed.insert(index, path).is_some() {
                    return Err(format!("duplicate SWM volume index {index}"));
                }
            }
        }
        if indexed.get(&1) != Some(&source) {
            return Err("selected SWM is not the primary volume".into());
        }
        for expected in 1..=indexed.keys().next_back().copied().unwrap_or(0) {
            if !indexed.contains_key(&expected) {
                return Err(format!("missing SWM volume index {expected}"));
            }
        }
        return Ok(indexed.into_values().collect());
    }

    let mut indexed = BTreeMap::from([(0usize, source.clone())]);
    for entry in std::fs::read_dir(parent).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if !extension_is(&path, "ghs") {
            continue;
        }
        let Some(candidate) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let index = if candidate.eq_ignore_ascii_case(stem) {
            Some(1)
        } else {
            strip_prefix_ascii_case(candidate, stem)
                .filter(|suffix| !suffix.is_empty())
                .filter(|suffix| suffix.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|suffix| suffix.parse::<usize>().ok().map(|index| (suffix, index)))
                .filter(|(suffix, index)| {
                    *index >= 1
                        && (*suffix == index.to_string() || *suffix == format!("{index:03}"))
                })
                .map(|(_, index)| index)
        };
        if let Some(index) = index {
            if indexed.insert(index, path).is_some() {
                return Err(format!("duplicate Ghost span index {index}"));
            }
        }
    }
    for expected in 0..=indexed.keys().next_back().copied().unwrap_or(0) {
        if !indexed.contains_key(&expected) {
            return Err(format!("missing Ghost span index {expected}"));
        }
    }
    let _ = name;
    Ok(indexed.into_values().collect())
}

#[cfg(windows)]
fn open_locked(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ};
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN.0);
    options.open(path)
}

fn hash_handle(file: &File) -> std::io::Result<String> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    crate::hash::sha256_reader(reader, |_| {})
}

impl LockedInstallSourceSet {
    /// Lock the original pathname set without creating a private staging copy. This is reserved
    /// for transactional backup output bases that must remain deny-write/delete locked until the
    /// final compare-and-publish boundary.
    pub fn acquire_pinned_original(source: &Path) -> Result<Self, String> {
        Self::acquire_unstaged(source, None)
    }

    pub fn acquire(source: &Path) -> Result<Self, String> {
        let original = Self::acquire_unstaged(source, None)?;
        if source_is_read_only_optical(original.selected_path())? {
            return Ok(original);
        }
        original.stage_private_copy()
    }

    fn acquire_unstaged(
        source: &Path,
        stage_dir: Option<crate::scoped_temp_file::ScopedTempDir>,
    ) -> Result<Self, String> {
        // Refuse reparse traversal before canonicalization. Otherwise an attacker could
        // redirect the pathname independently from the already-open child file handle.
        validate_non_reparse_path(source)?;
        let selected = std::fs::canonicalize(source)
            .map_err(|error| format!("canonicalize selected install source: {error}"))?;
        let paths = enumerate_install_image_set(&selected)?;
        let mut pinned_parents = Vec::new();
        let mut pinned_parent_paths = Vec::<PathBuf>::new();
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let canonical = std::fs::canonicalize(&path)
                .map_err(|error| format!("canonicalize install span: {error}"))?;
            let parent = canonical
                .parent()
                .ok_or_else(|| format!("install span has no parent: {}", canonical.display()))?;
            if !pinned_parent_paths.iter().any(|value| value == parent) {
                pinned_parents.push(
                    crate::scoped_temp_file::pin_existing_directory_ancestors(parent)
                        .map_err(|error| format!("pin install source ancestors: {error}"))?,
                );
                pinned_parent_paths.push(parent.to_path_buf());
            }
            let file = open_locked(&canonical)
                .map_err(|error| format!("lock install span {}: {error}", canonical.display()))?;
            let metadata = file.metadata().map_err(|error| {
                format!("inspect install span {}: {error}", canonical.display())
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "install span is not a regular file: {}",
                    canonical.display()
                ));
            }
            let sha256 = hash_handle(&file)
                .map_err(|error| format!("hash install span {}: {error}", canonical.display()))?;
            files.push(LockedFile {
                path: canonical,
                file,
                length: metadata.len(),
                sha256,
            });
        }
        Ok(Self {
            selected,
            files,
            pinned_parents,
            _stage_dir: stage_dir,
        })
    }

    fn stage_private_copy(&self) -> Result<Self, String> {
        let parent = self
            .selected
            .parent()
            .ok_or_else(|| "install source has no staging parent".to_string())?;
        let stage =
            crate::scoped_temp_file::ScopedTempDir::create_in(parent, "lr-install-source-stage")
                .map_err(|error| format!("create private install-source stage: {error}"))?;
        #[cfg(not(test))]
        {
            crate::scoped_temp_file::restrict_to_system_and_administrators(stage.path())
                .map_err(|error| format!("protect private install-source stage: {error}"))?;
            prove_directory_rename_is_blocked(stage.path())?;
        }

        let mut staged_selected = None;
        for locked in &self.files {
            self.verify_unchanged()?;
            let name = locked.path.file_name().ok_or_else(|| {
                format!(
                    "install source span has no file name: {}",
                    locked.path.display()
                )
            })?;
            let destination = stage.path().join(name);
            if std::fs::hard_link(&locked.path, &destination).is_err() {
                let mut reader = locked.file.try_clone().map_err(|error| error.to_string())?;
                reader
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| error.to_string())?;
                let mut writer = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|error| {
                        format!(
                            "stage install span {} after hard-link fallback: {error}",
                            destination.display()
                        )
                    })?;
                let copied = std::io::copy(&mut reader, &mut writer)
                    .map_err(|error| format!("copy install span into stage: {error}"))?;
                writer.flush().map_err(|error| error.to_string())?;
                writer.sync_all().map_err(|error| error.to_string())?;
                if copied != locked.length {
                    return Err(format!(
                        "short install-source staging copy: {}",
                        destination.display()
                    ));
                }
            }
            let staged = open_locked(&destination)
                .map_err(|error| format!("lock staged install span: {error}"))?;
            if staged.metadata().map_err(|error| error.to_string())?.len() != locked.length
                || hash_handle(&staged).map_err(|error| error.to_string())? != locked.sha256
            {
                return Err(format!(
                    "staged install span does not match its locked source: {}",
                    destination.display()
                ));
            }
            if locked.path == self.selected {
                staged_selected = Some(destination);
            }
        }
        self.verify_unchanged()?;
        let selected = staged_selected.ok_or_else(|| {
            "selected install source was not published into the private stage".to_string()
        })?;
        Self::acquire_unstaged(&selected, Some(stage))
    }

    /// Canonical, non-reparse pathname whose ancestors are pinned until this guard drops.
    pub fn selected_path(&self) -> &Path {
        &self.selected
    }

    /// Return the exact ordered span set represented by this guard.
    pub fn artifact_identities(&self) -> Result<Vec<LockedSourceArtifactIdentity>, String> {
        self.verify_unchanged()?;
        self.files
            .iter()
            .map(|locked| {
                Ok(LockedSourceArtifactIdentity {
                    path: locked.path.clone(),
                    length_bytes: locked.length,
                    sha256: crate::install_handoff::decode_hex_array::<32>(
                        &locked.sha256,
                        "locked install artifact SHA-256",
                    )
                    .map_err(|error| error.to_string())?,
                })
            })
            .collect()
    }

    /// Copy one member of the exact ordered image set from the same deny-write/delete handle
    /// retained by this guard.  The caller owns the destination handle and is responsible for
    /// creating it inside a protected session directory.
    pub fn copy_artifact_to_verified_writer(
        &self,
        ordinal: usize,
        destination: &mut File,
    ) -> Result<(), String> {
        self.copy_artifact_to_verified_writer_with_progress(ordinal, destination, |_| Ok(()))
    }

    pub fn copy_artifact_to_verified_writer_with_progress<F>(
        &self,
        ordinal: usize,
        destination: &mut File,
        mut progress: F,
    ) -> Result<(), String>
    where
        F: FnMut(u64) -> std::io::Result<()>,
    {
        self.verify_unchanged()?;
        let locked = self
            .files
            .get(ordinal)
            .ok_or_else(|| format!("install source ordinal {ordinal} is out of range"))?;
        let mut source = locked
            .file
            .try_clone()
            .map_err(|error| format!("clone locked install span handle: {error}"))?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("rewind locked install span: {error}"))?;
        destination
            .set_len(0)
            .map_err(|error| format!("truncate protected install span: {error}"))?;
        destination
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("rewind protected install span: {error}"))?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut copied = 0_u64;
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|error| format!("read locked install span: {error}"))?;
            if read == 0 {
                break;
            }
            destination
                .write_all(&buffer[..read])
                .map_err(|error| format!("write protected install span: {error}"))?;
            copied = copied
                .checked_add(read as u64)
                .ok_or_else(|| "protected install span copy length overflow".to_owned())?;
            progress(copied).map_err(|error| format!("copy install span progress: {error}"))?;
        }
        destination
            .flush()
            .map_err(|error| format!("flush protected install span: {error}"))?;
        destination
            .sync_all()
            .map_err(|error| format!("sync protected install span: {error}"))?;
        if copied != locked.length {
            return Err(format!(
                "protected install span length mismatch at ordinal {ordinal}"
            ));
        }
        let digest = hash_handle(destination).map_err(|error| error.to_string())?;
        if digest != locked.sha256 {
            return Err(format!(
                "protected install span SHA-256 mismatch at ordinal {ordinal}"
            ));
        }
        self.verify_unchanged()
    }

    /// Length and complete SHA-256 of the selected file, computed from the same deny-write/delete
    /// handle retained by this guard.
    pub fn selected_file_identity(
        &self,
    ) -> Result<crate::backup_handoff::BackupBaseFileIdentity, String> {
        let selected = self
            .files
            .iter()
            .find(|file| file.path == self.selected)
            .ok_or_else(|| "locked source set does not contain its selected file".to_owned())?;
        Ok(crate::backup_handoff::BackupBaseFileIdentity {
            length_bytes: selected.length,
            sha256: crate::install_handoff::decode_hex_array::<32>(
                &selected.sha256,
                "locked source SHA-256",
            )
            .map_err(|error| error.to_string())?,
        })
    }

    pub fn verify_unchanged(&self) -> Result<(), String> {
        for pinned in &self.pinned_parents {
            pinned
                .verify_unchanged()
                .map_err(|error| format!("install source ancestor changed: {error}"))?;
        }
        let current = enumerate_install_image_set(&self.selected)?;
        if current.len() != self.files.len() {
            return Err("install source span set changed after verification".into());
        }
        for (path, locked) in current.iter().zip(&self.files) {
            let canonical = std::fs::canonicalize(path)
                .map_err(|error| format!("rebind install span: {error}"))?;
            let metadata = locked.file.metadata().map_err(|error| error.to_string())?;
            let sha256 = hash_handle(&locked.file).map_err(|error| error.to_string())?;
            if canonical != locked.path
                || metadata.len() != locked.length
                || sha256 != locked.sha256
            {
                return Err(format!(
                    "install source changed after verification: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

/// Prove that an engine-visible directory contains exactly the ordered WIM/ESD/SWM or GHO/GHS
/// set authenticated by the handoff manifest.  This deliberately reuses the same strict naming
/// and contiguity rules as `LockedInstallSourceSet`; any additional matching sibling is rejected.
pub fn verify_exact_install_image_span_paths(
    selected: &Path,
    expected: &[PathBuf],
) -> Result<(), String> {
    if expected.is_empty() {
        return Err("authenticated install image set is empty".to_owned());
    }
    let actual = enumerate_install_image_set(selected)?;
    if actual.len() != expected.len() {
        return Err(
            "authenticated install image span inventory contains missing or extra files".to_owned(),
        );
    }
    for (ordinal, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        let expected = std::fs::canonicalize(expected)
            .map_err(|error| format!("canonicalize expected install span {ordinal}: {error}"))?;
        if *actual != expected {
            return Err(format!(
                "authenticated install image span order/path mismatch at ordinal {ordinal}"
            ));
        }
    }
    Ok(())
}

/// Prove that a protected engine-visible session directory contains no entry beyond the exact
/// manifest span set. This is required for external engines such as Ghost that discover GHS
/// siblings internally and therefore cannot accept an explicit resource list.
pub fn verify_engine_visible_directory_contains_only(expected: &[PathBuf]) -> Result<(), String> {
    if expected.is_empty() {
        return Err("authenticated install image set is empty".to_owned());
    }
    let mut canonical_expected = Vec::with_capacity(expected.len());
    let mut common_parent: Option<PathBuf> = None;
    for (ordinal, path) in expected.iter().enumerate() {
        let canonical = std::fs::canonicalize(path)
            .map_err(|error| format!("canonicalize expected install span {ordinal}: {error}"))?;
        let parent = canonical
            .parent()
            .ok_or_else(|| format!("install span {ordinal} has no parent directory"))?;
        match &common_parent {
            Some(expected_parent) if expected_parent != parent => {
                return Err("authenticated install spans do not share one directory".to_owned())
            }
            None => common_parent = Some(parent.to_path_buf()),
            _ => {}
        }
        canonical_expected.push(canonical);
    }
    canonical_expected.sort();

    let parent = common_parent.expect("non-empty expected set has a parent");
    let mut actual = Vec::new();
    for entry in std::fs::read_dir(&parent)
        .map_err(|error| format!("enumerate engine-visible install directory: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("read engine-visible directory entry: {error}"))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("inspect engine-visible directory entry: {error}"))?;
        if metadata_is_reparse(&metadata) || !metadata.is_file() {
            return Err(format!(
                "engine-visible install directory contains a non-regular entry: {}",
                entry.path().display()
            ));
        }
        actual.push(
            std::fs::canonicalize(entry.path())
                .map_err(|error| format!("canonicalize engine-visible entry: {error}"))?,
        );
    }
    actual.sort();
    if actual != canonical_expected {
        return Err(
            "engine-visible install directory contains a missing or manifest-external entry"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(windows)]
fn source_is_read_only_optical(path: &Path) -> Result<bool, String> {
    use std::path::{Component, Prefix};
    let drive = path.components().find_map(|component| match component {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter as char),
            _ => None,
        },
        _ => None,
    });
    let Some(drive) = drive else {
        return Ok(false);
    };
    crate::windows_storage::drive_kind(drive)
        .map(|kind| kind == crate::windows_storage::DriveKind::Optical)
        .map_err(|error| format!("classify install-source drive: {error}"))
}

#[cfg(not(windows))]
fn source_is_read_only_optical(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(not(test))]
fn prove_directory_rename_is_blocked(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "private stage has no parent".to_string())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "private stage has no Unicode name".to_string())?;
    let probe = parent.join(format!("{name}.rename-probe"));
    let pin = crate::scoped_temp_file::pin_existing_directory_ancestors(path)
        .map_err(|error| format!("pin private source stage: {error}"))?;
    match std::fs::rename(path, &probe) {
        Err(_) => {
            pin.verify_unchanged()
                .map_err(|error| format!("private source stage identity changed: {error}"))?;
            Ok(())
        }
        Ok(()) => {
            drop(pin);
            let restore = std::fs::rename(&probe, path);
            match restore {
                Ok(()) => Err(
                    "filesystem permits renaming a pinned source directory; refusing pathname-based image apply"
                        .into(),
                ),
                Err(error) => Err(format!(
                    "filesystem permits renaming a pinned source directory and the safety probe could not restore it: {error}"
                )),
            }
        }
    }
}

impl LockedInstallTree {
    pub fn acquire(selected: &Path) -> Result<Self, String> {
        validate_non_reparse_path(selected)?;
        let selected = std::fs::canonicalize(selected)
            .map_err(|error| format!("canonicalize XP source: {error}"))?;
        if !selected.is_dir() {
            return Err("XP source is not a directory".into());
        }
        let mut roots = vec![selected.clone()];
        if selected
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("AMD64"))
        {
            if let Some(parent) = selected.parent() {
                let sibling = parent.join("I386");
                if sibling.is_dir() {
                    validate_non_reparse_path(&sibling)?;
                    roots.push(std::fs::canonicalize(sibling).map_err(|error| error.to_string())?);
                }
            }
        }
        let mut directories = Vec::new();
        let mut files = Vec::new();
        for root in &roots {
            for entry in walkdir::WalkDir::new(root).follow_links(false) {
                let entry = entry.map_err(|error| format!("walk locked XP source: {error}"))?;
                let metadata = std::fs::symlink_metadata(entry.path())
                    .map_err(|error| format!("inspect XP source entry: {error}"))?;
                if metadata_is_reparse(&metadata) {
                    return Err(format!(
                        "XP source contains a reparse point: {}",
                        entry.path().display()
                    ));
                }
                if metadata.is_dir() {
                    directories.push(
                        std::fs::canonicalize(entry.path()).map_err(|error| error.to_string())?,
                    );
                } else if metadata.is_file() {
                    if files.len() >= MAX_LOCKED_TREE_FILES {
                        return Err(format!("XP source exceeds {MAX_LOCKED_TREE_FILES} files"));
                    }
                    let path =
                        std::fs::canonicalize(entry.path()).map_err(|error| error.to_string())?;
                    let file = open_locked(&path).map_err(|error| {
                        format!("lock XP source file {}: {error}", path.display())
                    })?;
                    files.push(LockedTreeFile {
                        path,
                        length: metadata.len(),
                        sha256: hash_handle(&file).map_err(|error| error.to_string())?,
                        file,
                    });
                } else {
                    return Err(format!(
                        "unsupported XP source entry: {}",
                        entry.path().display()
                    ));
                }
            }
        }
        directories.sort();
        directories.dedup();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let parent = selected
            .parent()
            .ok_or_else(|| "XP source has no parent".to_string())?;
        let pinned_parents =
            vec![
                crate::scoped_temp_file::pin_existing_directory_ancestors(parent)
                    .map_err(|error| format!("pin XP source ancestors: {error}"))?,
            ];
        Ok(Self {
            selected,
            roots,
            directories,
            files,
            pinned_parents,
        })
    }

    pub fn selected_path(&self) -> &Path {
        &self.selected
    }

    /// Return every file in the locked XP/2003 tree in canonical path order.
    pub fn artifact_identities(&self) -> Result<Vec<LockedSourceArtifactIdentity>, String> {
        self.verify_unchanged()?;
        self.files
            .iter()
            .map(|locked| {
                Ok(LockedSourceArtifactIdentity {
                    path: locked.path.clone(),
                    length_bytes: locked.length,
                    sha256: crate::install_handoff::decode_hex_array::<32>(
                        &locked.sha256,
                        "locked XP artifact SHA-256",
                    )
                    .map_err(|error| error.to_string())?,
                })
            })
            .collect()
    }

    fn locked_file(&self, path: &Path) -> Result<&LockedTreeFile, String> {
        let candidate = path.to_string_lossy();
        self.files
            .iter()
            .find(|locked| {
                locked
                    .path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&candidate)
            })
            .ok_or_else(|| {
                format!(
                    "XP source path is not present in the locked manifest: {}",
                    path.display()
                )
            })
    }

    pub fn contains_file(&self, path: &Path) -> bool {
        self.locked_file(path).is_ok()
    }

    pub fn contains_directory(&self, path: &Path) -> bool {
        let candidate = path.to_string_lossy();
        self.directories
            .iter()
            .any(|directory| directory.to_string_lossy().eq_ignore_ascii_case(&candidate))
    }

    /// Read bytes from the already-open manifest handle, never by reopening the source path.
    pub fn read_file(&self, path: &Path) -> Result<Vec<u8>, String> {
        let locked = self.locked_file(path)?;
        let capacity = usize::try_from(locked.length)
            .map_err(|_| format!("XP source file is too large to buffer: {}", path.display()))?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut reader = locked.file.try_clone().map_err(|error| error.to_string())?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != locked.length {
            return Err(format!(
                "short read from locked XP source: {}",
                path.display()
            ));
        }
        Ok(bytes)
    }

    fn copy_locked_file(&self, locked: &LockedTreeFile, destination: &Path) -> Result<(), String> {
        crate::windows_file_copy::reject_existing_reparse_ancestors(destination)
            .map_err(|error| error.to_string())?;
        if let Ok(metadata) = std::fs::symlink_metadata(destination) {
            if metadata_is_reparse(&metadata) || !metadata.is_file() {
                return Err(format!(
                    "refusing to overwrite non-regular XP destination: {}",
                    destination.display()
                ));
            }
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            crate::windows_file_copy::reject_existing_reparse_ancestors(parent)
                .map_err(|error| error.to_string())?;
        }
        let mut reader = locked.file.try_clone().map_err(|error| error.to_string())?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let mut writer = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(destination)
            .map_err(|error| format!("open XP destination {}: {error}", destination.display()))?;
        let copied = std::io::copy(&mut reader, &mut writer).map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
        writer.sync_all().map_err(|error| error.to_string())?;
        if copied != locked.length {
            return Err(format!(
                "short copy into XP destination: {}",
                destination.display()
            ));
        }
        let metadata = writer.metadata().map_err(|error| error.to_string())?;
        if metadata.len() != locked.length {
            return Err(format!(
                "XP destination length mismatch: {}",
                destination.display()
            ));
        }
        Ok(())
    }

    /// Copy one source file from its locked manifest handle.
    pub fn copy_file(&self, source: &Path, destination: &Path) -> Result<(), String> {
        self.copy_locked_file(self.locked_file(source)?, destination)
    }

    /// Copy exactly the entries recorded below `source_root`; late additions are never consumed.
    pub fn copy_tree(
        &self,
        source_root: &Path,
        destination: &Path,
        continue_on_error: bool,
    ) -> Result<crate::windows_file_copy::CopyTreeReport, String> {
        let candidate = source_root.to_string_lossy();
        let source_root = self
            .directories
            .iter()
            .find(|directory| directory.to_string_lossy().eq_ignore_ascii_case(&candidate))
            .ok_or_else(|| {
                format!(
                    "XP source subtree is not present in the locked manifest: {}",
                    source_root.display()
                )
            })?;
        crate::windows_file_copy::reject_existing_reparse_ancestors(destination)
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(destination).map_err(|error| error.to_string())?;
        let mut report = crate::windows_file_copy::CopyTreeReport::default();
        for directory in &self.directories {
            let Ok(relative) = directory.strip_prefix(source_root) else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let target = destination.join(relative);
            let result = crate::windows_file_copy::reject_existing_reparse_ancestors(&target)
                .and_then(|()| std::fs::create_dir_all(&target).map_err(anyhow::Error::from));
            if let Err(error) = result {
                if continue_on_error {
                    report.errors.push(error.to_string());
                } else {
                    return Err(error.to_string());
                }
            }
        }
        for locked in &self.files {
            let Ok(relative) = locked.path.strip_prefix(source_root) else {
                continue;
            };
            let target = destination.join(relative);
            match self.copy_locked_file(locked, &target) {
                Ok(()) => report.files_copied += 1,
                Err(error) if continue_on_error => report.errors.push(error),
                Err(error) => return Err(error),
            }
        }
        Ok(report)
    }

    pub fn verify_unchanged(&self) -> Result<(), String> {
        for pinned in &self.pinned_parents {
            pinned
                .verify_unchanged()
                .map_err(|error| error.to_string())?;
        }
        let mut current_dirs = Vec::new();
        let mut current_files = Vec::new();
        for root in &self.roots {
            for entry in walkdir::WalkDir::new(root).follow_links(false) {
                let entry = entry.map_err(|error| error.to_string())?;
                let metadata =
                    std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
                if metadata_is_reparse(&metadata) {
                    return Err(format!(
                        "XP source gained a reparse point: {}",
                        entry.path().display()
                    ));
                }
                let path =
                    std::fs::canonicalize(entry.path()).map_err(|error| error.to_string())?;
                if metadata.is_dir() {
                    current_dirs.push(path);
                } else if metadata.is_file() {
                    current_files.push(path);
                }
            }
        }
        current_dirs.sort();
        current_dirs.dedup();
        current_files.sort();
        if current_dirs != self.directories
            || current_files
                != self
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>()
        {
            return Err("XP source tree manifest changed after verification".into());
        }
        for locked in &self.files {
            let metadata = locked.file.metadata().map_err(|error| error.to_string())?;
            if metadata.len() != locked.length
                || hash_handle(&locked.file).map_err(|error| error.to_string())? != locked.sha256
            {
                return Err(format!("XP source file changed: {}", locked.path.display()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_source_is_enumerated_as_one_canonical_file() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-single",
        )
        .unwrap();
        let source = temp.path().join("install.wim");
        std::fs::write(&source, b"wim").unwrap();
        assert_eq!(
            enumerate_install_image_set(&source).unwrap(),
            vec![std::fs::canonicalize(source).unwrap()]
        );
    }

    #[test]
    fn split_wim_requires_contiguous_indices() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-split",
        )
        .unwrap();
        let source = temp.path().join("install.swm");
        std::fs::write(&source, b"one").unwrap();
        std::fs::write(temp.path().join("install3.swm"), b"three").unwrap();
        assert!(enumerate_install_image_set(&source)
            .unwrap_err()
            .contains("missing SWM"));
    }

    #[test]
    fn exact_split_manifest_rejects_an_unlisted_late_span() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-exact-split",
        )
        .unwrap();
        let source = temp.path().join("install.swm");
        let second = temp.path().join("install2.swm");
        std::fs::write(&source, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let expected = vec![
            std::fs::canonicalize(&source).unwrap(),
            std::fs::canonicalize(&second).unwrap(),
        ];
        verify_exact_install_image_span_paths(&source, &expected).unwrap();

        std::fs::write(temp.path().join("install3.swm"), b"late").unwrap();
        assert!(verify_exact_install_image_span_paths(&source, &expected)
            .unwrap_err()
            .contains("missing or extra"));
    }

    #[test]
    fn exact_ghost_manifest_rejects_an_unlisted_late_span() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-exact-ghost",
        )
        .unwrap();
        let source = temp.path().join("system.gho");
        let second = temp.path().join("system001.ghs");
        std::fs::write(&source, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let expected = vec![
            std::fs::canonicalize(&source).unwrap(),
            std::fs::canonicalize(&second).unwrap(),
        ];
        verify_exact_install_image_span_paths(&source, &expected).unwrap();

        std::fs::write(temp.path().join("system002.ghs"), b"late").unwrap();
        assert!(verify_exact_install_image_span_paths(&source, &expected)
            .unwrap_err()
            .contains("missing or extra"));
    }

    #[test]
    fn protected_swm_directory_rejects_a_glob_only_manifest_external_file() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-full-directory-swm",
        )
        .unwrap();
        let source = temp.path().join("install.swm");
        let second = temp.path().join("install2.swm");
        std::fs::write(&source, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let expected = vec![
            std::fs::canonicalize(&source).unwrap(),
            std::fs::canonicalize(&second).unwrap(),
        ];
        verify_engine_visible_directory_contains_only(&expected).unwrap();

        // This spelling is deliberately not a canonical ordinal, but it matched the old
        // `install*.swm` engine glob and therefore must be rejected by the full inventory.
        std::fs::write(temp.path().join("install03.swm"), b"external").unwrap();
        assert!(verify_engine_visible_directory_contains_only(&expected)
            .unwrap_err()
            .contains("manifest-external"));
    }

    #[test]
    fn protected_ghost_directory_rejects_every_non_manifest_entry() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-full-directory-ghost",
        )
        .unwrap();
        let source = temp.path().join("system.gho");
        let second = temp.path().join("system001.ghs");
        std::fs::write(&source, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let expected = vec![
            std::fs::canonicalize(&source).unwrap(),
            std::fs::canonicalize(&second).unwrap(),
        ];
        verify_engine_visible_directory_contains_only(&expected).unwrap();

        std::fs::write(temp.path().join("system999.ghs"), b"external").unwrap();
        assert!(verify_engine_visible_directory_contains_only(&expected)
            .unwrap_err()
            .contains("manifest-external"));
    }

    #[test]
    fn locked_split_copy_uses_all_held_handles_in_manifest_order() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-copy-split",
        )
        .unwrap();
        let source = temp.path().join("install.swm");
        std::fs::write(&source, b"one").unwrap();
        std::fs::write(temp.path().join("install2.swm"), b"two").unwrap();
        let guard = LockedInstallSourceSet::acquire_pinned_original(&source).unwrap();
        let destination = temp.path().join("copied.swm");
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap();
        guard
            .copy_artifact_to_verified_writer(1, &mut output)
            .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"two");
    }

    #[cfg(windows)]
    #[test]
    fn held_guard_rejects_write_and_path_replacement() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-sharing",
        )
        .unwrap();
        let parent = temp.path().join("source");
        std::fs::create_dir(&parent).unwrap();
        let source = parent.join("install.wim");
        std::fs::write(&source, b"verified").unwrap();
        let guard = LockedInstallSourceSet::acquire(&source).unwrap();
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .is_err());
        let renamed = temp.path().join("renamed-source");
        match std::fs::rename(&parent, &renamed) {
            Err(_) => guard.verify_unchanged().unwrap(),
            Ok(()) => {
                std::fs::create_dir(&parent).unwrap();
                std::fs::write(&source, b"replacement").unwrap();
                assert!(guard.verify_unchanged().is_err());
                drop(guard);
                std::fs::remove_dir_all(&parent).unwrap();
                std::fs::remove_dir_all(&renamed).unwrap();
            }
        }
    }

    #[test]
    fn locked_xp_tree_copies_only_manifest_handles_and_detects_late_entries() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-xp-tree",
        )
        .unwrap();
        let source = temp.path().join("I386");
        std::fs::create_dir_all(source.join("SYSTEM32")).unwrap();
        std::fs::write(source.join("setupldr.bin"), b"loader").unwrap();
        std::fs::write(source.join("SYSTEM32").join("kernel.dll"), b"kernel").unwrap();
        let guard = LockedInstallTree::acquire(&source).unwrap();

        std::fs::write(source.join("late.inf"), b"late").unwrap();
        assert!(guard
            .verify_unchanged()
            .unwrap_err()
            .contains("manifest changed"));

        let destination = temp.path().join("destination");
        let report = guard
            .copy_tree(guard.selected_path(), &destination, false)
            .unwrap();
        assert_eq!(report.files_copied, 2);
        assert_eq!(
            std::fs::read(destination.join("setupldr.bin")).unwrap(),
            b"loader"
        );
        assert!(!destination.join("late.inf").exists());
    }

    #[test]
    fn plain_artifact_snapshot_is_copied_from_the_held_handle() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-plain-copy",
        )
        .unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("snapshot.bin");
        std::fs::write(&source, b"authenticated bytes").unwrap();
        let guard = LockedPlainArtifact::acquire(&source).unwrap();
        let mut output = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&destination)
            .unwrap();
        guard.copy_to_verified_writer(&mut output).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"authenticated bytes");
        guard.verify_unchanged().unwrap();
    }

    #[test]
    fn plain_artifact_authentication_reports_exact_bytes_and_allows_lightweight_recheck() {
        let temp = crate::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-source-lock-progress",
        )
        .unwrap();
        let source = temp.path().join("source.bin");
        let bytes = vec![0x5a; (1 << 20) + 37];
        std::fs::write(&source, &bytes).unwrap();
        let mut samples = Vec::new();
        let guard =
            LockedPlainArtifact::acquire_with_progress(&source, |read| samples.push(read)).unwrap();

        assert_eq!(samples.last().copied(), Some(bytes.len() as u64));
        assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(guard.identity().length_bytes, bytes.len() as u64);
        guard.verify_binding_unchanged().unwrap();
    }
}
