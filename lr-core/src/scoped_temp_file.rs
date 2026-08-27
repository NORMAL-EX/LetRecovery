//! Collision-resistant temporary regular files with best-effort cleanup.

use std::ffi::OsString;
use std::fs::{remove_dir_all, remove_file, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);
const MAX_CREATE_ATTEMPTS: u64 = 128;

/// Read a small control file through a handle that neither follows a final reparse point nor
/// permits concurrent write/delete replacement. The byte limit is checked before allocation and
/// again after reading.
pub fn read_bounded_plain_file(path: &Path, maximum_bytes: u64) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
        options
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control path is not a plain regular file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "control path is a reparse point",
            ));
        }
    }
    if metadata.len() > maximum_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control file exceeds its byte limit",
        ));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "control file length is not addressable",
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes || file.metadata()?.len() != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control file changed or exceeded its byte limit during read",
        ));
    }
    Ok(bytes)
}

/// Read a control file while every existing directory ancestor is held without delete sharing.
///
/// The returned pins must remain alive for as long as the caller relies on the pathname. This
/// closes directory-junction and ancestor-rename substitution around configuration/marker reads;
/// callers performing a later mutation must call `verify_unchanged` immediately before and after
/// that mutation.
pub fn read_bounded_plain_file_pinned(
    path: &Path,
    maximum_bytes: u64,
) -> io::Result<(Vec<u8>, PinnedDirectoryAncestors)> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "control file path has no parent directory",
        )
    })?;
    let pins = pin_existing_directory_ancestors(parent)?;
    pins.verify_unchanged()?;
    let bytes = read_bounded_plain_file(path, maximum_bytes)?;
    pins.verify_unchanged()?;
    Ok((bytes, pins))
}

/// A temporary regular file that is removed when the guard is dropped.
///
/// `create_new` prevents one process instance from overwriting another one's
/// command script. Cleanup also runs when command startup or output handling
/// returns early with an error.
#[derive(Debug)]
pub struct ScopedTempFile {
    path: PathBuf,
}

/// A collision-resistant temporary directory removed recursively on drop.
#[derive(Debug)]
pub struct ScopedTempDir {
    path: PathBuf,
    custody: Option<SecureDirectoryCustody>,
}

/// Identity-pins a private directory for the lifetime of its cleanup guard. The root handle is
/// opened without delete sharing; descendant handles are retained for the same reason until
/// cleanup starts.
#[derive(Debug)]
struct SecureDirectoryCustody {
    root: File,
    identity: DirectoryIdentity,
    ancestors: PinnedDirectoryAncestors,
    descendants: Vec<SecureDirectoryDescendant>,
}

#[derive(Debug)]
struct SecureDirectoryDescendant {
    path: PathBuf,
    handle: File,
    identity: DirectoryIdentity,
}

/// Holds and identity-pins every currently existing directory component.
///
/// On Windows this uses documented `CreateFileW` directory-handle semantics through
/// `OpenOptionsExt`: `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`, read-attributes
/// access, and read/write/delete sharing. Microsoft documents that delete access also covers
/// rename and that every existing handle must grant `FILE_SHARE_DELETE`; omitting it from a
/// read-only identity pin caused supported descendant namespace publication to fail in WinPE with
/// `ERROR_SHARING_VIOLATION`. These handles are identity evidence, not locks: callers must invoke
/// `verify_unchanged` immediately after any pathname operation and before writing sensitive bytes.
/// Missing descendants are deliberately skipped; callers must pin again after creating them.
#[derive(Debug)]
pub struct PinnedDirectoryAncestors {
    entries: Vec<PinnedDirectoryAncestor>,
}

#[derive(Debug)]
struct PinnedDirectoryAncestor {
    path: PathBuf,
    _handle: File,
    identity: DirectoryIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    volume: u64,
    file: u64,
}

#[cfg(windows)]
fn open_pinned_directory(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let handle = options.open(path)?;
    if handle.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "pinned path ancestor is a reparse point: {}",
                path.display()
            ),
        ));
    }
    Ok(handle)
}

#[cfg(not(windows))]
fn open_pinned_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(windows)]
fn directory_identity(file: &File) -> io::Result<DirectoryIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information)
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    }
    Ok(DirectoryIdentity {
        volume: information.dwVolumeSerialNumber as u64,
        file: ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
    })
}

#[cfg(not(windows))]
fn directory_identity(file: &File) -> io::Result<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(DirectoryIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

impl PinnedDirectoryAncestors {
    pub fn verify_unchanged(&self) -> io::Result<()> {
        for entry in &self.entries {
            let current = open_pinned_directory(&entry.path)?;
            if directory_identity(&current)? != entry.identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "pinned path ancestor identity changed: {}",
                        entry.path.display()
                    ),
                ));
            }
        }
        Ok(())
    }
}

pub fn pin_existing_directory_ancestors(path: &Path) -> io::Result<PinnedDirectoryAncestors> {
    let mut ancestors = path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .collect::<Vec<_>>();
    ancestors.reverse();
    let mut entries = Vec::with_capacity(ancestors.len());
    for ancestor in ancestors {
        let metadata = match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "pinned path ancestor is not a regular directory: {}",
                    ancestor.display()
                ),
            ));
        }
        let handle = open_pinned_directory(ancestor)?;
        let identity = directory_identity(&handle)?;
        entries.push(PinnedDirectoryAncestor {
            path: ancestor.to_path_buf(),
            _handle: handle,
            identity,
        });
    }
    Ok(PinnedDirectoryAncestors { entries })
}

/// Pin only the ancestors above `path`, excluding `path` itself.
///
/// This is for operations that retain their own exact handle to `path` and must legitimately
/// modify names immediately inside it.  Holding `path` itself without `FILE_SHARE_DELETE` would
/// make that supported namespace mutation fail on WinPE.  The immediate parent remains pinned,
/// which prevents `path` from being renamed or replaced, while the caller must re-open `path`
/// and compare its file ID at every mutation boundary.
pub fn pin_existing_parent_directory_ancestors(
    path: &Path,
) -> io::Result<PinnedDirectoryAncestors> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(PinnedDirectoryAncestors {
            entries: Vec::new(),
        });
    };
    pin_existing_directory_ancestors(parent)
}

impl ScopedTempDir {
    pub fn create_in(parent: &Path, prefix: &str) -> io::Result<Self> {
        validate_name_component(prefix, "prefix")?;
        std::fs::create_dir_all(parent)?;
        let parent_metadata = std::fs::symlink_metadata(parent)?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary directory parent is not a regular directory",
            ));
        }
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        custody: None,
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary directory",
        ))
    }

    /// Create a collision-safe private directory whose security descriptor is installed by
    /// `CreateDirectoryW` before its name becomes visible.
    ///
    /// On Windows the owner and primary group are BUILTIN\Administrators and the protected DACL
    /// contains exactly two full-control allow ACEs: local SYSTEM and
    /// BUILTIN\Administrators. The returned guard keeps an identity-pinned directory handle open
    /// without delete sharing. The existing parent and all existing ancestors are pinned during
    /// creation and their ACLs are never modified.
    pub fn create_system_administrators_in(parent: &Path, prefix: &str) -> io::Result<Self> {
        validate_name_component(prefix, "prefix")?;
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "secure directory parent is not an ordinary directory",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "secure directory parent is a reparse point",
                ));
            }
        }

        let pins = pin_existing_directory_ancestors(parent)?;
        pins.verify_unchanged()?;
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}-{}-{id}", std::process::id()));
            let root = match create_system_administrators_directory_new(&path) {
                Ok(root) => root,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            if let Err(error) = pins.verify_unchanged() {
                drop(root);
                let cleanup = std::fs::remove_dir(&path);
                return Err(with_cleanup_error(
                    error,
                    cleanup,
                    "secure directory ancestor verification failed",
                ));
            }
            let identity = match directory_identity(&root) {
                Ok(identity) => identity,
                Err(error) => {
                    let cleanup = delete_directory_by_handle(&root, &path);
                    drop(root);
                    return Err(with_cleanup_error(
                        error,
                        cleanup,
                        "secure directory identity query failed",
                    ));
                }
            };
            return Ok(Self {
                path,
                custody: Some(SecureDirectoryCustody {
                    root,
                    identity,
                    ancestors: pins,
                    descendants: Vec::new(),
                }),
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique SYSTEM/Administrators protected temporary directory",
        ))
    }

    /// Atomically create every component of a relative directory below this private root.
    ///
    /// Every component receives the same creation-time custody descriptor and is verified through
    /// the exact handle before the next component is created. Absolute paths, `.`/`..`, Windows
    /// device names, alternate-data-stream syntax, trailing dot/space names, separators and
    /// control characters are rejected. A prefix previously created and still identity-pinned by
    /// this same guard may be reused for a sibling branch; arbitrary existing components are never
    /// adopted.
    pub fn create_system_administrators_subdirectory(
        &mut self,
        relative: &Path,
    ) -> io::Result<PathBuf> {
        let components = validate_secure_relative_directory(relative)?;
        self.verify_system_administrators_custody()?;
        let mut created = Vec::with_capacity(components.len());
        let mut handles = Vec::with_capacity(components.len());
        let mut current = self.path.clone();
        for component in components {
            current.push(component);
            if let Some(existing) = self.custody.as_ref().and_then(|custody| {
                custody
                    .descendants
                    .iter()
                    .find(|entry| entry.path == current)
            }) {
                verify_system_administrators_directory_custody(&existing.handle)?;
                if directory_identity(&existing.handle)? != existing.identity {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retained secure subdirectory handle identity changed",
                    ));
                }
                let reopened = open_custody_directory_readback(&current)?;
                if directory_identity(&reopened)? != existing.identity {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retained secure subdirectory pathname was rebound",
                    ));
                }
                continue;
            }
            match create_system_administrators_directory_new(&current) {
                Ok(handle) => {
                    created.push(current.clone());
                    let identity = match directory_identity(&handle) {
                        Ok(identity) => identity,
                        Err(error) => {
                            drop(handle);
                            drop(handles);
                            let cleanup = remove_empty_directories_reverse(&created);
                            return Err(with_cleanup_error(
                                error,
                                cleanup,
                                "secure relative directory identity query failed",
                            ));
                        }
                    };
                    handles.push(SecureDirectoryDescendant {
                        path: current.clone(),
                        handle,
                        identity,
                    });
                }
                Err(error) => {
                    drop(handles);
                    let cleanup = remove_empty_directories_reverse(&created);
                    return Err(with_cleanup_error(
                        error,
                        cleanup,
                        "secure relative directory creation failed",
                    ));
                }
            }
        }
        if let Err(error) = self.verify_system_administrators_custody() {
            drop(handles);
            let cleanup = remove_empty_directories_reverse(&created);
            return Err(with_cleanup_error(
                error,
                cleanup,
                "secure directory identity changed during relative creation",
            ));
        }
        self.custody
            .as_mut()
            .expect("custody was verified above")
            .descendants
            .extend(handles);
        Ok(current)
    }

    /// Recheck the root through both its retained handle and its current pathname.
    pub fn verify_system_administrators_custody(&self) -> io::Result<()> {
        let custody = self.custody.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary directory was not created by the secure custody API",
            )
        })?;
        custody.ancestors.verify_unchanged()?;
        verify_system_administrators_directory_custody(&custody.root)?;
        let current = open_custody_directory_readback(&self.path)?;
        if directory_identity(&current)? != custody.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "secure directory pathname no longer identifies the retained directory",
            ));
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the directory on disk and return its path.
    ///
    /// Use this only when ownership is intentionally transferred to another
    /// component that will clean the directory after consuming its contents.
    pub fn into_path(mut self) -> PathBuf {
        self.custody.take();
        let path = std::mem::take(&mut self.path);
        std::mem::forget(self);
        path
    }

    /// Transfer the already verified private-directory handle to a long-lived owner.
    ///
    /// The returned handle is the same handle opened with `DELETE` access immediately after
    /// `CreateDirectoryW`. Callers retain that verified object identity instead of discarding it
    /// and trusting a later pathname reopen. The handle permits delete sharing because WinPE file
    /// systems may internally reopen a rename target directory; the long-lived owner must pair it
    /// with pathname-to-file-ID readback at every mutation boundary.
    pub fn into_system_administrators_directory(mut self) -> io::Result<(PathBuf, File)> {
        self.verify_system_administrators_custody()?;
        let custody = self.custody.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary directory was not created by the secure custody API",
            )
        })?;
        if !custody.descendants.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot transfer a secure directory while descendant custody handles are retained",
            ));
        }

        let SecureDirectoryCustody {
            root,
            identity: _,
            ancestors: _,
            descendants: _,
        } = self
            .custody
            .take()
            .expect("secure custody was verified above");
        let path = std::mem::take(&mut self.path);
        std::mem::forget(self);
        Ok((path, root))
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        if let Some(mut custody) = self.custody.take() {
            if let Err(error) = self.verify_retained_directory_identity(&custody) {
                log::warn!(
                    "secure temporary directory identity changed; preserving {}: {}",
                    self.path.display(),
                    error
                );
                return;
            }
            // Release descendant custody handles before removing the tree while retaining the
            // root handle as the pathname identity pin.
            custody.descendants.clear();
            if let Err(error) = remove_temporary_tree_contents(&self.path) {
                log::warn!(
                    "secure temporary directory content cleanup failed for {}: {}",
                    self.path.display(),
                    error
                );
                return;
            }
            if let Err(error) = self.verify_retained_directory_identity(&custody) {
                log::warn!(
                    "secure temporary directory identity changed after content cleanup; preserving {}: {}",
                    self.path.display(),
                    error
                );
                return;
            }
            if let Err(error) = delete_directory_by_handle(&custody.root, &self.path) {
                log::warn!(
                    "secure temporary directory handle cleanup failed for {}: {}",
                    self.path.display(),
                    error
                );
            }
            return;
        }
        if let Err(error) = remove_temporary_tree(&self.path) {
            log::warn!(
                "temporary directory cleanup failed for {}: {}",
                self.path.display(),
                error
            );
        }
    }
}

impl ScopedTempDir {
    fn verify_retained_directory_identity(
        &self,
        custody: &SecureDirectoryCustody,
    ) -> io::Result<()> {
        verify_system_administrators_directory_custody(&custody.root)?;
        custody.ancestors.verify_unchanged()?;
        if directory_identity(&custody.root)? != custody.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retained secure directory handle identity changed",
            ));
        }
        let current = open_custody_directory_readback(&self.path)?;
        if directory_identity(&current)? != custody.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "secure directory path was rebound to another object",
            ));
        }
        Ok(())
    }
}

fn with_cleanup_error(error: io::Error, cleanup: io::Result<()>, context: &str) -> io::Error {
    match cleanup {
        Ok(()) => io::Error::new(error.kind(), format!("{context}: {error}")),
        Err(cleanup_error) => io::Error::new(
            error.kind(),
            format!("{context}: {error}; cleanup failed: {cleanup_error}"),
        ),
    }
}

fn remove_empty_directories_reverse(paths: &[PathBuf]) -> io::Result<()> {
    let mut first_error = None;
    for path in paths.iter().rev() {
        if let Err(error) = std::fs::remove_dir(path) {
            if error.kind() != io::ErrorKind::NotFound && first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn validate_secure_relative_directory(relative: &Path) -> io::Result<Vec<OsString>> {
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure subdirectory path must be a non-empty relative path",
        ));
    }
    let mut result = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "secure subdirectory path contains a root, prefix, dot, or parent component",
            ));
        };
        validate_windows_directory_component(name)?;
        result.push(name.to_os_string());
    }
    if result.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure subdirectory path has no components",
        ));
    }
    Ok(result)
}

fn validate_windows_directory_component(name: &std::ffi::OsStr) -> io::Result<()> {
    let value = name.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure subdirectory component is not valid Unicode",
        )
    })?;
    if value.is_empty()
        || value.ends_with(['.', ' '])
        || value.chars().any(|character| {
            character <= '\u{1f}'
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure subdirectory component uses Windows-unsafe syntax",
        ));
    }
    let stem = value.split('.').next().unwrap_or(value).to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || reserved_numbered_device(&stem, "COM")
        || reserved_numbered_device(&stem, "LPT");
    if reserved {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure subdirectory component is a reserved Windows device name",
        ));
    }
    Ok(())
}

fn reserved_numbered_device(value: &str, prefix: &str) -> bool {
    let Some(suffix) = value.strip_prefix(prefix) else {
        return false;
    };
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn remove_temporary_tree_contents(path: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let child = entry?.path();
        let metadata = std::fs::symlink_metadata(&child)?;
        if metadata.is_dir() && !metadata_is_reparse(&metadata) {
            remove_temporary_tree(&child)?;
        } else {
            let first = if metadata.is_dir() {
                std::fs::remove_dir(&child)
            } else {
                remove_file(&child)
            };
            if let Err(first_error) = first {
                clear_tree_removal_attributes(&child)?;
                let retry = if metadata.is_dir() {
                    std::fs::remove_dir(&child)
                } else {
                    remove_file(&child)
                };
                if let Err(retry_error) = retry {
                    return Err(io::Error::new(
                        retry_error.kind(),
                        format!(
                            "initial child removal failed: {first_error}; retry failed: {retry_error}"
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn delete_directory_by_handle(directory: &File, _path: &Path) -> io::Result<()> {
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
            HANDLE(directory.as_raw_handle()),
            FileDispositionInfo,
            (&information as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))
}

#[cfg(not(windows))]
fn delete_directory_by_handle(_directory: &File, path: &Path) -> io::Result<()> {
    std::fs::remove_dir(path)
}

/// Remove a private temporary directory, retrying after clearing Windows
/// read-only/system/hidden attributes commonly restored from WIM metadata.
/// Reparse points are never traversed while attributes are cleared.
pub fn remove_temporary_tree(path: &Path) -> io::Result<()> {
    match remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(first_error) => {
            if path.exists() {
                clear_tree_removal_attributes(path)?;
            }
            match remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(retry_error) => Err(io::Error::new(
                    retry_error.kind(),
                    format!(
                        "initial removal failed: {first_error}; retry after clearing attributes failed: {retry_error}"
                    ),
                )),
            }
        }
    }
}

#[cfg(windows)]
fn clear_tree_removal_attributes(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM,
        FILE_FLAGS_AND_ATTRIBUTES, INVALID_FILE_ATTRIBUTES,
    };

    fn attributes(path: &Path) -> io::Result<u32> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let value = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
        if value == INVALID_FILE_ATTRIBUTES {
            Err(io::Error::last_os_error())
        } else {
            Ok(value)
        }
    }

    fn clear_one(path: &Path, current: u32) -> io::Result<()> {
        let mut cleared = current
            & !(FILE_ATTRIBUTE_READONLY.0 | FILE_ATTRIBUTE_SYSTEM.0 | FILE_ATTRIBUTE_HIDDEN.0);
        if cleared == 0 {
            cleared = FILE_ATTRIBUTE_NORMAL.0;
        }
        if cleared == current {
            return Ok(());
        }
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(cleared))
                .map_err(|_| io::Error::last_os_error())
        }
    }

    let current = attributes(path)?;
    if current & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
        && std::fs::symlink_metadata(path)?.file_type().is_dir()
    {
        for entry in std::fs::read_dir(path)? {
            clear_tree_removal_attributes(&entry?.path())?;
        }
    }
    clear_one(path, current)
}

#[cfg(not(windows))]
fn clear_tree_removal_attributes(_path: &Path) -> io::Result<()> {
    Ok(())
}

impl ScopedTempFile {
    pub fn create_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
        contents: &[u8],
    ) -> io::Result<Self> {
        let (guard, mut file) = Self::create_writer_in(directory, prefix, extension)?;
        if let Err(error) = file.write_all(contents).and_then(|_| file.flush()) {
            drop(file);
            drop(guard);
            return Err(error);
        }
        drop(file);
        Ok(guard)
    }

    /// Allocate a unique temporary file and return both its cleanup guard and
    /// writable handle. Callers can stream large payloads without buffering
    /// them in memory; dropping the guard removes partial files on failure.
    pub fn create_writer_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
    ) -> io::Result<(Self, File)> {
        validate_name_component(prefix, "prefix")?;
        validate_name_component(extension, "extension")?;

        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let file_name = format!("{prefix}-{}-{id}.{extension}", std::process::id());
            let path = directory.join(file_name);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
                // Keep delete sharing denied so the pathname cannot be replaced, while allowing
                // the owner to reopen the same object to install its protected DACL.
                options.share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0);
            }
            let file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };

            return Ok((Self { path }, file));
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary file",
        ))
    }

    /// Create a collision-safe file whose protected DACL is installed atomically at creation.
    /// The DACL grants full control only to the unique SID set consisting of the current token
    /// user, SYSTEM and Administrators; when the token user is SYSTEM, `SET_ACCESS` may consolidate
    /// the two entries for that same trustee.
    #[cfg(windows)]
    pub fn create_protected_writer_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
    ) -> io::Result<(Self, File)> {
        validate_name_component(prefix, "prefix")?;
        validate_name_component(extension, "extension")?;
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("{prefix}-{}-{id}.{extension}", std::process::id()));
            match create_protected_interactive_file_new(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique protected temporary file",
        ))
    }

    #[cfg(not(windows))]
    pub fn create_protected_writer_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
    ) -> io::Result<(Self, File)> {
        Self::create_writer_in(directory, prefix, extension)
    }

    /// Create a collision-safe file that is already in elevated-process custody when its name
    /// first becomes visible. Its owner and primary group are BUILTIN\Administrators and its
    /// protected DACL contains exactly two full-control allow ACEs: local SYSTEM and
    /// BUILTIN\Administrators.
    ///
    /// This boundary is for LRPE4 capsules, private boot WIMs and other artifacts that must not
    /// briefly inherit a writable parent ACL. It intentionally does not grant the interactive
    /// user access and therefore requires an elevated administrator or SYSTEM token.
    #[cfg(windows)]
    pub fn create_system_administrators_writer_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
    ) -> io::Result<(Self, File)> {
        validate_name_component(prefix, "prefix")?;
        validate_name_component(extension, "extension")?;
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("{prefix}-{}-{id}.{extension}", std::process::id()));
            match create_system_administrators_file_new(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique SYSTEM/Administrators protected temporary file",
        ))
    }

    #[cfg(not(windows))]
    pub fn create_system_administrators_writer_in(
        directory: &Path,
        prefix: &str,
        extension: &str,
    ) -> io::Result<(Self, File)> {
        Self::create_writer_in(directory, prefix, extension)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically publish this temporary file at `target`.
    ///
    /// The temporary file and target must be in the same directory so the
    /// replacement cannot degrade into a cross-volume copy. On Windows this
    /// uses `MoveFileExW` with replace and write-through semantics.
    pub fn persist_replace(self, target: &Path) -> io::Result<()> {
        if self.path.parent() != target.parent() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file and target must share a directory",
            ));
        }
        atomic_replace_path(&self.path, target)
    }
}

#[cfg(windows)]
fn create_protected_interactive_file_new(path: &Path) -> io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    struct LocalGuard(HLOCAL);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(self.0);
            }
        }
    }

    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    let _token = HandleGuard(token);
    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(io::Error::last_os_error());
    }
    let mut bytes = vec![0u8; needed as usize];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(bytes.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    let record = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(record.User.Sid, &mut sid_text) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    let _sid_guard = LocalGuard(HLOCAL(sid_text.0.cast()));
    let mut length = 0usize;
    while unsafe { *sid_text.0.add(length) } != 0 {
        length += 1;
    }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text.0, length) })
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "current user SID is not valid UTF-16",
            )
        })?;
    let sddl = format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;BA)");
    let sddl = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    let _descriptor_guard = LocalGuard(HLOCAL(descriptor.0));
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

#[cfg(windows)]
struct LocalSecurityDescriptor(windows::Win32::Security::PSECURITY_DESCRIPTOR);

#[cfg(windows)]
impl LocalSecurityDescriptor {
    fn system_administrators() -> io::Result<Self> {
        use windows::core::PCWSTR;
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows::Win32::Security::PSECURITY_DESCRIPTOR;

        // Owner and group are set explicitly so an elevated administrator-created file cannot
        // retain the token's interactive user as owner (owners have implicit WRITE_DAC rights).
        let sddl = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        Ok(Self(descriptor))
    }
}

#[cfg(windows)]
impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        if !self.0 .0.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.0 .0));
            }
        }
    }
}

/// Create a named ordinary file with CREATE_NEW and the strict LRPE4 custody descriptor installed
/// by `CreateFileW` itself. The returned handle is verified before the function returns.
#[cfg(windows)]
pub fn create_system_administrators_file_new(path: &Path) -> io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let descriptor = LocalSecurityDescriptor::system_administrators()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0 .0,
        bInheritHandle: false.into(),
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    let file = unsafe { File::from_raw_handle(handle.0) };
    if let Err(error) = verify_system_administrators_file_custody(&file) {
        drop(file);
        let cleanup = remove_file(path);
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(io::Error::new(
                error.kind(),
                format!(
                    "secure file custody readback failed: {error}; cleanup failed: {cleanup_error}"
                ),
            )),
        };
    }
    Ok(file)
}

/// Create a directory with the strict custody descriptor installed atomically, then open it
/// with delete sharing and verify that exact directory object through the returned handle.
#[cfg(windows)]
fn create_system_administrators_directory_new(path: &Path) -> io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;

    let descriptor = LocalSecurityDescriptor::system_administrators()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0 .0,
        bInheritHandle: false.into(),
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&attributes)) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;

    let directory = match open_custody_directory(path) {
        Ok(directory) => directory,
        Err(error) => {
            let cleanup = std::fs::remove_dir(path);
            return Err(with_cleanup_error(
                error,
                cleanup,
                "secure directory open after creation failed",
            ));
        }
    };
    if let Err(error) = verify_system_administrators_directory_custody(&directory) {
        drop(directory);
        let cleanup = std::fs::remove_dir(path);
        return Err(with_cleanup_error(
            error,
            cleanup,
            "secure directory custody readback failed",
        ));
    }
    Ok(directory)
}

#[cfg(not(windows))]
fn create_system_administrators_directory_new(path: &Path) -> io::Result<File> {
    std::fs::create_dir(path)?;
    open_custody_directory(path)
}

#[cfg(windows)]
fn open_custody_directory(path: &Path) -> io::Result<File> {
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
    let directory = options.open(path)?;
    let metadata = directory.metadata()?;
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure custody path is not an ordinary non-reparse directory",
        ));
    }
    Ok(directory)
}

/// Reopen a private directory only for pathname/identity/custody readback while its retained
/// identity handle remains alive. Attribute and security-descriptor reads do not need `DELETE`.
#[cfg(windows)]
fn open_custody_directory_readback(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    let directory = options.open(path)?;
    let metadata = directory.metadata()?;
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure custody readback path is not an ordinary non-reparse directory",
        ));
    }
    Ok(directory)
}

#[cfg(not(windows))]
fn open_custody_directory(path: &Path) -> io::Result<File> {
    let directory = OpenOptions::new().read(true).open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure custody path is not an ordinary directory",
        ));
    }
    Ok(directory)
}

#[cfg(not(windows))]
fn open_custody_directory_readback(path: &Path) -> io::Result<File> {
    open_custody_directory(path)
}

/// Open an existing ordinary directory without delete sharing and prove that its owner/group and
/// protected DACL still match the SYSTEM/Administrators custody contract.  Keeping the returned
/// handle alive pins the exact namespace object while a path-based third-party consumer runs.
pub fn open_system_administrators_directory(path: &Path) -> io::Result<File> {
    let directory = open_custody_directory(path)?;
    verify_system_administrators_directory_custody(&directory)?;
    Ok(directory)
}

#[cfg(not(windows))]
pub fn create_system_administrators_file_new(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

impl AsRef<Path> for ScopedTempFile {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl std::ops::Deref for ScopedTempFile {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

/// Atomically replace `target` with an already-complete file on the same
/// volume. Callers must keep `source` in a private staging directory and must
/// verify it before publishing.
#[cfg(windows)]
pub fn atomic_replace_path(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))
    }
}

/// Atomically publish a complete same-volume file only when the destination is still absent.
/// This is the create-only counterpart to [`atomic_replace_path`]; it deliberately omits
/// `MOVEFILE_REPLACE_EXISTING`, closing the check-then-rename overwrite race.
#[cfg(windows)]
pub fn atomic_publish_new_path(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))
    }
}

/// Atomically replace `target` while preserving the exact previous file at `backup` until the
/// caller completes post-publication readback. All three paths must reside on the same volume.
#[cfg(windows)]
pub fn atomic_replace_path_with_backup(
    source: &Path,
    target: &Path,
    backup: &Path,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

    if backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "atomic replacement backup path already exists",
        ));
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let backup: Vec<u16> = backup.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(source.as_ptr()),
            PCWSTR(backup.as_ptr()),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))
    }
}

#[cfg(not(windows))]
pub fn atomic_replace_path_with_backup(
    source: &Path,
    target: &Path,
    backup: &Path,
) -> io::Result<()> {
    if backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "atomic replacement backup path already exists",
        ));
    }
    std::fs::rename(target, backup)?;
    match std::fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::rename(backup, target);
            Err(error)
        }
    }
}

#[cfg(not(windows))]
pub fn atomic_publish_new_path(source: &Path, target: &Path) -> io::Result<()> {
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "create-only publish destination already exists",
        ));
    }
    std::fs::rename(source, target)
}

#[cfg(windows)]
fn well_known_sid_bytes(
    kind: windows::Win32::Security::WELL_KNOWN_SID_TYPE,
) -> io::Result<Vec<u8>> {
    use windows::Win32::Security::{CreateWellKnownSid, PSID, SECURITY_MAX_SID_SIZE};
    let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = buffer.len() as u32;
    unsafe {
        CreateWellKnownSid(
            kind,
            PSID::default(),
            PSID(buffer.as_mut_ptr().cast()),
            &mut size,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    buffer.truncate(size as usize);
    Ok(buffer)
}

#[cfg(all(test, windows))]
pub(crate) fn test_token_is_elevated_administrator() -> io::Result<bool> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{CheckTokenMembership, WinBuiltinAdministratorsSid, PSID};
    let administrators = well_known_sid_bytes(WinBuiltinAdministratorsSid)?;
    let mut is_member = false.into();
    unsafe {
        CheckTokenMembership(
            HANDLE::default(),
            PSID(administrators.as_ptr().cast_mut().cast()),
            &mut is_member,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    Ok(is_member.as_bool())
}

/// Strictly verify the security descriptor of the exact open file object.
///
/// The owner and group must each be local SYSTEM or BUILTIN\Administrators, the DACL must be
/// protected, and it must contain exactly one non-inherited full-control allow ACE for each of
/// those two SIDs. In particular, a user-owned file is rejected even when its DACL appears
/// restricted, because the Windows owner can rewrite that DACL.
#[cfg(windows)]
pub fn verify_system_administrators_file_custody(file: &File) -> io::Result<()> {
    verify_system_administrators_handle_custody(file, false)
}

/// Strictly verify the security descriptor and object type of an exact open directory handle.
#[cfg(windows)]
pub fn verify_system_administrators_directory_custody(directory: &File) -> io::Result<()> {
    verify_system_administrators_handle_custody(directory, true)
}

#[cfg(windows)]
fn verify_system_administrators_handle_custody(
    file: &File,
    expect_directory: bool,
) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        IsValidSid, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACL,
        ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    };
    use windows::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, FILE_ATTRIBUTE_REPARSE_POINT};

    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || (expect_directory && !metadata.is_dir())
        || (!expect_directory && !metadata.is_file())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            if expect_directory {
                "secure custody handle is not an ordinary non-reparse directory"
            } else {
                "secure custody handle is not an ordinary non-reparse file"
            },
        ));
    }

    struct DescriptorGuard(PSECURITY_DESCRIPTOR);
    impl Drop for DescriptorGuard {
        fn drop(&mut self) {
            if !self.0 .0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 .0));
                }
            }
        }
    }

    let mut owner = PSID::default();
    let mut group = PSID::default();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let result = unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            Some(&mut group),
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    let _descriptor = DescriptorGuard(descriptor);
    if owner.is_invalid()
        || group.is_invalid()
        || !unsafe { IsValidSid(owner).as_bool() }
        || !unsafe { IsValidSid(group).as_bool() }
        || dacl.is_null()
        || descriptor.0.is_null()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "secure custody descriptor omits owner, group, or DACL",
        ));
    }

    let administrators = well_known_sid_bytes(WinBuiltinAdministratorsSid)?;
    let system = well_known_sid_bytes(WinLocalSystemSid)?;
    let administrators_sid = PSID(administrators.as_ptr().cast_mut().cast());
    let system_sid = PSID(system.as_ptr().cast_mut().cast());
    let is_custodian = |sid: PSID| unsafe {
        EqualSid(sid, administrators_sid).is_ok() || EqualSid(sid, system_sid).is_ok()
    };
    if !is_custodian(owner) || !is_custodian(group) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "secure custody owner/group is not SYSTEM or BUILTIN\\Administrators",
        ));
    }

    let mut control = 0u16;
    let mut revision = 0u32;
    unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    if control & SE_DACL_PROTECTED.0 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "secure custody DACL is not protected from inheritance",
        ));
    }

    let mut information = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    if information.AceCount != 2 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "secure custody DACL does not contain exactly two ACEs",
        ));
    }

    let mut saw_administrators = false;
    let mut saw_system = false;
    for index in 0..information.AceCount {
        let mut raw_ace = null_mut();
        unsafe { GetAce(dacl, index, &mut raw_ace) }
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        if raw_ace.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "secure custody DACL returned a null ACE",
            ));
        }
        let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Header.AceType != 0
            || ace.Header.AceFlags != 0
            || usize::from(ace.Header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            || ace.Mask != FILE_ALL_ACCESS.0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "secure custody DACL contains a non-exact allow ACE",
            ));
        }
        let sid = PSID((&ace.SidStart as *const u32).cast_mut().cast());
        if !unsafe { IsValidSid(sid).as_bool() } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "secure custody DACL contains an invalid SID",
            ));
        }
        if unsafe { EqualSid(sid, administrators_sid).is_ok() } {
            if saw_administrators {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "secure custody DACL contains a duplicate Administrators ACE",
                ));
            }
            saw_administrators = true;
        } else if unsafe { EqualSid(sid, system_sid).is_ok() } {
            if saw_system {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "secure custody DACL contains a duplicate SYSTEM ACE",
                ));
            }
            saw_system = true;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "secure custody DACL grants an unexpected principal",
            ));
        }
    }
    if !saw_administrators || !saw_system {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "secure custody DACL omits SYSTEM or BUILTIN\\Administrators",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn verify_system_administrators_file_custody(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn verify_system_administrators_directory_custody(directory: &File) -> io::Result<()> {
    if directory.metadata()?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure custody handle is not a directory",
        ))
    }
}

#[cfg(windows)]
fn apply_system_administrators_file_custody(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        GetSecurityDescriptorDacl, GetSecurityDescriptorGroup, GetSecurityDescriptorOwner,
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    };

    let descriptor = LocalSecurityDescriptor::system_administrators()?;
    let mut owner = PSID::default();
    let mut group = PSID::default();
    let mut dacl = std::ptr::null_mut();
    let mut defaulted = false.into();
    let mut present = false.into();
    unsafe {
        GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted)
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        GetSecurityDescriptorGroup(descriptor.0, &mut group, &mut defaulted)
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    }
    if owner.is_invalid() || group.is_invalid() || !present.as_bool() || dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "constructed secure custody descriptor is incomplete",
        ));
    }
    let result = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | GROUP_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            group,
            Some(dacl.cast_const()),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    verify_system_administrators_file_custody(file)
}

/// Replace a file's inherited ACL and owner with the strict SYSTEM/Administrators custody
/// descriptor. This path-based compatibility helper opens the ordinary file once, mutates that
/// exact handle, and verifies the owner and DACL through the same handle. New LRPE4 artifacts
/// must instead use `create_system_administrators_file_new` so there is no inheritance window.
#[cfg(windows)]
pub fn restrict_to_system_and_administrators(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    };

    let mut options = OpenOptions::new();
    options
        .access_mode(READ_CONTROL.0 | WRITE_DAC.0 | WRITE_OWNER.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = options.open(path)?;
    use std::os::windows::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure custody target is not an ordinary non-reparse file",
        ));
    }
    apply_system_administrators_file_custody(&file)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveFileAclPrincipal {
    CurrentUser,
    LocalSystem,
    BuiltinAdministrators,
}

const INTERACTIVE_FILE_ACL_PRINCIPALS: [InteractiveFileAclPrincipal; 3] = [
    InteractiveFileAclPrincipal::CurrentUser,
    InteractiveFileAclPrincipal::LocalSystem,
    InteractiveFileAclPrincipal::BuiltinAdministrators,
];

/// Replace a file's inherited ACL with a protected DACL granting full control only to the
/// unique SID set consisting of the current token user, local SYSTEM, and BUILTIN\Administrators.
/// `SET_ACCESS` may consolidate duplicate trustees when the token user is SYSTEM. This is intended for a secret
/// file created by an asInvoker interactive process: the user must retain access while service
/// and later elevated LetRecovery processes can consume it. This function never changes a parent
/// directory ACL.
#[cfg(windows)]
pub fn restrict_to_current_user_system_and_administrators(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
        SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_IS_WELL_KNOWN_GROUP,
        TRUSTEE_W,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, GetTokenInformation, TokenUser, WinBuiltinAdministratorsSid,
        WinLocalSystemSid, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
        SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    struct AclGuard(*mut windows::Win32::Security::ACL);
    impl Drop for AclGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0.cast()));
                }
            }
        }
    }
    fn well_known_sid(kind: windows::Win32::Security::WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = buffer.len() as u32;
        unsafe {
            CreateWellKnownSid(
                kind,
                PSID::default(),
                PSID(buffer.as_mut_ptr().cast()),
                &mut size,
            )
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        }
        buffer.truncate(size as usize);
        Ok(buffer)
    }

    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    }
    let _token_guard = HandleGuard(token);
    let mut token_user_bytes = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut token_user_bytes) };
    if token_user_bytes < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(io::Error::last_os_error());
    }
    let mut token_user = vec![0u8; token_user_bytes as usize];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(token_user.as_mut_ptr().cast()),
            token_user_bytes,
            &mut token_user_bytes,
        )
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    }
    // `Vec<u8>` has byte alignment, so reading `TOKEN_USER` through a normal
    // dereference would be undefined behaviour on targets that require pointer
    // alignment. The SID itself remains owned by `token_user` for this whole call.
    let token_user_record =
        unsafe { std::ptr::read_unaligned(token_user.as_ptr().cast::<TOKEN_USER>()) };
    let current_user_sid = token_user_record.User.Sid;
    if current_user_sid.is_invalid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TokenUser returned an invalid SID",
        ));
    }
    let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
    let system = well_known_sid(WinLocalSystemSid)?;
    let sid_entries = [
        (current_user_sid.0.cast::<u8>(), TRUSTEE_IS_USER),
        (system.as_ptr().cast_mut(), TRUSTEE_IS_WELL_KNOWN_GROUP),
        (
            administrators.as_ptr().cast_mut(),
            TRUSTEE_IS_WELL_KNOWN_GROUP,
        ),
    ];
    debug_assert_eq!(sid_entries.len(), INTERACTIVE_FILE_ACL_PRINCIPALS.len());
    let access = sid_entries.map(|(sid, trustee_type)| EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS.0,
        grfAccessMode: SET_ACCESS,
        grfInheritance: Default::default(),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: trustee_type,
            ptstrName: PWSTR(sid.cast()),
        },
    });
    let mut acl = null_mut();
    let result = unsafe { SetEntriesInAclW(Some(&access), None, &mut acl) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    let _acl_guard = AclGuard(acl);
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            PSID::default(),
            PSID::default(),
            Some(acl.cast_const()),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn restrict_to_current_user_system_and_administrators(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn restrict_to_system_and_administrators(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod interactive_acl_tests {
    use super::*;

    #[test]
    fn interactive_secret_acl_has_exactly_the_three_documented_principals() {
        assert_eq!(
            INTERACTIVE_FILE_ACL_PRINCIPALS,
            [
                InteractiveFileAclPrincipal::CurrentUser,
                InteractiveFileAclPrincipal::LocalSystem,
                InteractiveFileAclPrincipal::BuiltinAdministrators,
            ]
        );
    }

    #[cfg(windows)]
    fn security_descriptor_bytes(file: &File) -> io::Result<Vec<u8>> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
        use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
        use windows::Win32::Security::{
            GetSecurityDescriptorLength, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };

        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let result = unsafe {
            GetSecurityInfo(
                HANDLE(file.as_raw_handle()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut descriptor),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result.0 as i32));
        }
        if descriptor.0.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GetSecurityInfo returned a null descriptor",
            ));
        }
        let length = unsafe { GetSecurityDescriptorLength(descriptor) } as usize;
        let bytes =
            unsafe { std::slice::from_raw_parts(descriptor.0.cast::<u8>(), length) }.to_vec();
        unsafe {
            let _ = LocalFree(HLOCAL(descriptor.0));
        }
        Ok(bytes)
    }

    #[cfg(windows)]
    fn open_directory_security(path: &Path) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
        };
        let mut options = OpenOptions::new();
        options
            .access_mode(READ_CONTROL.0)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
        options.open(path)
    }

    #[cfg(windows)]
    fn token_is_elevated_administrator() -> io::Result<bool> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Security::{CheckTokenMembership, WinBuiltinAdministratorsSid, PSID};
        let administrators = well_known_sid_bytes(WinBuiltinAdministratorsSid)?;
        let mut is_member = false.into();
        unsafe {
            CheckTokenMembership(
                HANDLE::default(),
                PSID(administrators.as_ptr().cast_mut().cast()),
                &mut is_member,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
        Ok(is_member.as_bool())
    }

    #[test]
    #[cfg(windows)]
    fn user_owner_with_restricted_dacl_is_rejected() {
        let directory = std::env::temp_dir().join(format!(
            "lr-core-custody-negative-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("user-owned.bin");
        let file = create_protected_interactive_file_new(&path).unwrap();
        let error = verify_system_administrators_file_custody(&file).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("owner/group"));
        drop(file);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn secure_create_has_exact_custody_and_does_not_mutate_parent_acl() {
        if !token_is_elevated_administrator().unwrap() {
            // The production boundary deliberately cannot be exercised by a filtered/non-admin
            // token because that token is not allowed to assign BA as an object owner.
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "lr-core-custody-positive-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let parent_before = {
            let parent = open_directory_security(&directory).unwrap();
            security_descriptor_bytes(&parent).unwrap()
        };
        let (guard, file) = ScopedTempFile::create_system_administrators_writer_in(
            &directory,
            "lrpe4-capsule",
            "bin",
        )
        .unwrap();
        verify_system_administrators_file_custody(&file).unwrap();
        let parent_after = {
            let parent = open_directory_security(&directory).unwrap();
            security_descriptor_bytes(&parent).unwrap()
        };
        assert_eq!(parent_before, parent_after);
        drop(file);
        drop(guard);
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn secure_directory_create_subdirectories_and_drop_keep_exact_custody() {
        if !token_is_elevated_administrator().unwrap() {
            return;
        }
        let parent = std::env::temp_dir().join(format!(
            "lr-core-directory-custody-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&parent).unwrap();
        let parent_before = {
            let handle = open_directory_security(&parent).unwrap();
            security_descriptor_bytes(&handle).unwrap()
        };

        let mut guard = ScopedTempDir::create_system_administrators_in(&parent, "lrpe4").unwrap();
        let root = guard.path().to_path_buf();
        guard.verify_system_administrators_custody().unwrap();
        let nested = guard
            .create_system_administrators_subdirectory(Path::new("artifacts\\secrets"))
            .unwrap();
        let sibling = guard
            .create_system_administrators_subdirectory(Path::new("artifacts\\drivers"))
            .unwrap();
        let nested_handle = open_custody_directory_readback(&nested).unwrap();
        verify_system_administrators_directory_custody(&nested_handle).unwrap();
        drop(nested_handle);
        let sibling_handle = open_custody_directory_readback(&sibling).unwrap();
        verify_system_administrators_directory_custody(&sibling_handle).unwrap();
        drop(sibling_handle);

        let parent_after = {
            let handle = open_directory_security(&parent).unwrap();
            security_descriptor_bytes(&handle).unwrap()
        };
        assert_eq!(parent_before, parent_after);

        let renamed = root.with_extension("renamed");
        assert!(std::fs::rename(&root, &renamed).is_err());
        assert!(std::fs::remove_dir(&root).is_err());
        let renamed_parent = parent.with_extension("renamed");
        assert!(std::fs::rename(&parent, &renamed_parent).is_err());
        guard.verify_system_administrators_custody().unwrap();

        drop(guard);
        assert!(!root.exists());
        std::fs::rename(&parent, &renamed_parent).unwrap();
        std::fs::remove_dir(renamed_parent).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn secure_directory_transfer_keeps_identity_while_allowing_supported_rename() {
        if !token_is_elevated_administrator().unwrap() {
            return;
        }
        let parent = std::env::temp_dir().join(format!(
            "lr-core-directory-transfer-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&parent).unwrap();

        let guard = ScopedTempDir::create_system_administrators_in(&parent, "private").unwrap();
        let expected_path = guard.path().to_path_buf();
        let expected_identity = directory_identity(&guard.custody.as_ref().unwrap().root).unwrap();
        let (path, handle) = guard.into_system_administrators_directory().unwrap();

        assert_eq!(path, expected_path);
        assert_eq!(directory_identity(&handle).unwrap(), expected_identity);
        verify_system_administrators_directory_custody(&handle).unwrap();
        let renamed = path.with_extension("renamed");
        std::fs::rename(&path, &renamed).unwrap();
        assert_eq!(directory_identity(&handle).unwrap(), expected_identity);
        let reopened = open_custody_directory_readback(&renamed).unwrap();
        assert_eq!(directory_identity(&reopened).unwrap(), expected_identity);
        drop(reopened);
        delete_directory_by_handle(&handle, &renamed).unwrap();
        drop(handle);
        assert!(!renamed.exists());
        std::fs::remove_dir(parent).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn secure_directory_rejects_user_owner_reparse_and_unsafe_relative_paths() {
        let parent = std::env::temp_dir().join(format!(
            "lr-core-directory-negative-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&parent).unwrap();
        let user_owned = parent.join("user-owned");
        std::fs::create_dir(&user_owned).unwrap();
        let user_handle = open_custody_directory(&user_owned).unwrap();
        let error = verify_system_administrators_directory_custody(&user_handle).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(user_handle);

        if token_is_elevated_administrator().unwrap() {
            let mut guard =
                ScopedTempDir::create_system_administrators_in(&parent, "private").unwrap();
            for attack in [
                "..\\escape",
                ".\\escape",
                "CON",
                "safe\\LPT1.txt",
                "alternate:stream",
                "trailing. ",
            ] {
                assert!(guard
                    .create_system_administrators_subdirectory(Path::new(attack))
                    .is_err());
            }
            drop(guard);
        }

        let real = parent.join("real");
        let link = parent.join("link");
        std::fs::create_dir(&real).unwrap();
        if std::os::windows::fs::symlink_dir(&real, &link).is_ok() {
            assert!(ScopedTempDir::create_system_administrators_in(&link, "private").is_err());
            std::fs::remove_dir(&link).unwrap();
        }
        std::fs::remove_dir(real).unwrap();
        std::fs::remove_dir(user_owned).unwrap();
        std::fs::remove_dir(parent).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn secure_directory_drop_preserves_path_when_recorded_identity_is_not_exact() {
        if !token_is_elevated_administrator().unwrap() {
            return;
        }
        let parent = std::env::temp_dir().join(format!(
            "lr-core-directory-preserve-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&parent).unwrap();
        let mut guard = ScopedTempDir::create_system_administrators_in(&parent, "private").unwrap();
        let root = guard.path().to_path_buf();
        guard.custody.as_mut().unwrap().identity.file ^= 1;

        drop(guard);

        assert!(root.is_dir());
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(parent).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn path_compatibility_helper_sets_owner_and_exact_dacl() {
        if !token_is_elevated_administrator().unwrap() {
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "lr-core-custody-compat-{}-{}",
            std::process::id(),
            NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("secret.bin");
        drop(create_protected_interactive_file_new(&path).unwrap());
        restrict_to_system_and_administrators(&path).unwrap();
        let file = OpenOptions::new().read(true).open(&path).unwrap();
        verify_system_administrators_file_custody(&file).unwrap();
        drop(file);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}

#[cfg(not(windows))]
pub fn atomic_replace_path(source: &Path, target: &Path) -> io::Result<()> {
    std::fs::rename(source, target)
}

impl Drop for ScopedTempFile {
    fn drop(&mut self) {
        let _ = remove_file(&self.path);
    }
}

fn validate_name_component(value: &str, field: &str) -> io::Result<()> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("temporary file {field} contains unsafe characters"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory() -> PathBuf {
        let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lr-core-temp-test-{}-{id}", std::process::id()))
    }

    #[test]
    fn creates_unique_files_and_cleans_them_on_drop() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();

        let first = ScopedTempFile::create_in(&directory, "diskpart", "txt", b"first").unwrap();
        let second = ScopedTempFile::create_in(&directory, "diskpart", "txt", b"second").unwrap();
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();

        assert_ne!(first_path, second_path);
        assert_eq!(std::fs::read(&first_path).unwrap(), b"first");
        assert_eq!(std::fs::read(&second_path).unwrap(), b"second");

        drop(first);
        drop(second);
        assert!(!first_path.exists());
        assert!(!second_path.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rejects_path_components_in_generated_names() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();

        let error =
            ScopedTempFile::create_in(&directory, "../script", "txt", b"unsafe").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn writer_api_cleans_partial_streams() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let (guard, mut writer) =
            ScopedTempFile::create_writer_in(&directory, "download", "wim").unwrap();
        writer.write_all(b"partial").unwrap();
        writer.flush().unwrap();
        let path = guard.path().to_path_buf();
        drop(writer);
        drop(guard);
        assert!(!path.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn persist_replace_atomically_replaces_an_existing_file() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("config.json");
        std::fs::write(&target, b"old").unwrap();
        let replacement = ScopedTempFile::create_in(&directory, "config", "json", b"new").unwrap();

        replacement.persist_replace(&target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn create_only_publish_does_not_replace_an_existing_file() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("backup.wim");
        let staged = directory.join("staged.wim");
        std::fs::write(&target, b"original").unwrap();
        std::fs::write(&staged, b"replacement").unwrap();

        assert!(atomic_publish_new_path(&staged, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(std::fs::read(&staged).unwrap(), b"replacement");
        std::fs::remove_file(target).unwrap();
        std::fs::remove_file(staged).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn pinned_bounded_control_read_enforces_limit_and_keeps_ancestors_stable() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("control.ini");
        std::fs::write(&path, b"LRBK2").unwrap();

        let (bytes, pins) = read_bounded_plain_file_pinned(&path, 16).unwrap();
        assert_eq!(bytes, b"LRBK2");
        pins.verify_unchanged().unwrap();
        assert!(read_bounded_plain_file_pinned(&path, 4).is_err());

        drop(pins);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn pinned_directory_ancestor_allows_supported_rename_and_detects_rebinding() {
        let directory = test_directory();
        let renamed = directory.with_extension("renamed");
        let _ = std::fs::remove_dir(&directory);
        let _ = std::fs::remove_dir(&renamed);
        std::fs::create_dir(&directory).unwrap();
        let pins = pin_existing_directory_ancestors(&directory).unwrap();
        std::fs::rename(&directory, &renamed).unwrap();
        std::fs::create_dir(&directory).unwrap();
        assert!(pins.verify_unchanged().is_err());
        drop(pins);
        std::fs::remove_dir(&directory).unwrap();
        std::fs::remove_dir(&renamed).unwrap();
    }

    #[test]
    fn temp_dir_into_path_keeps_directory() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let temp = ScopedTempDir::create_in(&directory, "drivers").unwrap();
        let path = temp.into_path();

        assert!(path.is_dir());

        std::fs::remove_dir(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn removes_wim_style_read_only_temporary_tree() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        let temp = ScopedTempDir::create_in(&directory, "wim-extract").unwrap();
        let path = temp.path().to_path_buf();
        let nested = path.join("Windows").join("Boot").join("bootmgfw.efi");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, b"test").unwrap();
        let mut permissions = std::fs::metadata(&nested).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&nested, permissions).unwrap();

        drop(temp);

        assert!(!path.exists());
        std::fs::remove_dir(directory).unwrap();
    }
}
