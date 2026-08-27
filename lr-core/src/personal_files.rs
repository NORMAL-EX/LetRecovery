//! Same-volume personal-file preservation for an offline Windows reinstall.
//!
//! This is deliberately narrower than a backup: only the six conventional local profile
//! directories are moved aside. After every move succeeds, selected old-system roots are really
//! deleted so their space is immediately reusable by image application. At first logon, regular
//! files are copied into newly created destination objects before their preserved originals are
//! removed, so a reinstalled account receives the new profile's inherited filesystem ACLs.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const PERSONAL_DIRECTORY_NAMES: [&str; 6] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
];

const OLD_SYSTEM_DIRECTORIES: [&str; 11] = [
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "Users",
    "PerfLogs",
    "Recovery",
    "Documents and Settings",
    "$WINDOWS.~BT",
    "$Windows.~WS",
    "$WinREAgent",
];

const OLD_SYSTEM_FILES: [&str; 8] = [
    "hiberfil.sys",
    "pagefile.sys",
    "swapfile.sys",
    "DumpStack.log",
    "DumpStack.log.tmp",
    "bootmgr",
    "BOOTNXT",
    "bootsect.bak",
];

const MAX_SHELL_LINK_BYTES: u64 = 1024 * 1024;
const SHELL_LINK_HEADER_SIZE: usize = 0x4C;
const SHELL_LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

const EXCLUDED_PROFILE_NAMES: [&str; 7] = [
    "All Users",
    "Default",
    "Default User",
    "defaultuser0",
    "WDAGUtilityAccount",
    "LocalService",
    "NetworkService",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservedDirectory {
    pub profile_name: String,
    pub directory_name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservationPlan {
    pub target_root: PathBuf,
    pub preserved_root: PathBuf,
    pub directories: Vec<PreservedDirectory>,
    pub files: u64,
    pub bytes: u64,
    /// Destination paths of Desktop `.lnk` files whose raw target is outside `Users` on either
    /// the original C: drive or the authenticated offline target drive currently assigned by PE.
    pub desktop_shortcuts_to_remove: Vec<PathBuf>,
    /// Links that did not expose a file target are retained instead of being guessed.
    pub unresolved_desktop_shortcuts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreservationStage {
    Reversible,
    /// A pre-delete move failed and at least one completed move could not be restored. No old
    /// system tree was intentionally deleted, but callers must not claim a complete rollback.
    RollbackIncomplete,
    OldSystemDeletionStarted,
}

#[derive(Debug)]
pub struct PreservationFailure {
    pub stage: PreservationStage,
    pub preserved_root: PathBuf,
    pub error: anyhow::Error,
}

impl std::fmt::Display for PreservationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl std::error::Error for PreservationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservationReport {
    pub preserved_root: PathBuf,
    pub preserved_directories: usize,
    pub preserved_files: u64,
    pub preserved_bytes: u64,
    pub deleted_roots: usize,
    pub deleted_entries: u64,
    pub deleted_desktop_shortcuts: u64,
    pub unresolved_desktop_shortcuts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalFileRestoreReport {
    pub preserved_root: PathBuf,
    pub current_profile_root: PathBuf,
    /// Current-token Known Folder destinations in Desktop, Documents, Downloads, Pictures,
    /// Music, Videos order. These are authoritative even when Windows redirects a folder beneath
    /// OneDrive or another location inside the current profile.
    pub personal_directories: [PathBuf; PERSONAL_DIRECTORY_NAMES.len()],
    /// Public Known Folder destinations in the same fixed order.
    pub public_directories: [PathBuf; PERSONAL_DIRECTORY_NAMES.len()],
    pub source_profiles: usize,
    pub restored_directories: u64,
    pub restored_files: u64,
    pub renamed_conflicts: u64,
}

#[derive(Debug, Clone)]
struct RestoreKnownFolders {
    current_profile_root: PathBuf,
    personal: [PathBuf; 6],
    public: [PathBuf; 6],
}

#[derive(Debug, Default)]
struct RestoreCounters {
    directories: u64,
    files: u64,
    renamed_conflicts: u64,
}

/// Restores the current installation session's preserved personal files into the actual known
/// folders of the user who is completing first logon. The destination is deliberately resolved
/// from the current token instead of being guessed from the requested account name: Windows can
/// choose a different profile directory for collisions, localized built-in Administrator names,
/// or an explicitly redirected known folder.
#[cfg(windows)]
pub fn restore_preserved_personal_files_for_current_user(
    session_id: &str,
) -> Result<PersonalFileRestoreReport> {
    let (system_root, destinations) = current_user_restore_context()?;

    validate_session_id(session_id)?;
    let preserved_root = system_root.join(format!("LetRecovery_Preserved_{session_id}"));
    restore_preserved_personal_files_to(&preserved_root, &destinations)
}

#[cfg(windows)]
fn current_user_restore_context() -> Result<(PathBuf, RestoreKnownFolders)> {
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_PublicDesktop, FOLDERID_PublicDocuments,
        FOLDERID_PublicDownloads, FOLDERID_PublicMusic, FOLDERID_PublicPictures,
        FOLDERID_PublicVideos, FOLDERID_Videos, FOLDERID_Windows,
    };

    let windows = known_folder_path(&FOLDERID_Windows)?;
    let system_root = windows
        .parent()
        .ok_or_else(|| anyhow!("Windows known folder has no volume root"))?
        .to_path_buf();
    validate_target_root(&system_root)?;
    let destinations = RestoreKnownFolders {
        current_profile_root: known_folder_path(&FOLDERID_Profile)?,
        personal: [
            known_folder_path(&FOLDERID_Desktop)?,
            known_folder_path(&FOLDERID_Documents)?,
            known_folder_path(&FOLDERID_Downloads)?,
            known_folder_path(&FOLDERID_Pictures)?,
            known_folder_path(&FOLDERID_Music)?,
            known_folder_path(&FOLDERID_Videos)?,
        ],
        public: [
            known_folder_path(&FOLDERID_PublicDesktop)?,
            known_folder_path(&FOLDERID_PublicDocuments)?,
            known_folder_path(&FOLDERID_PublicDownloads)?,
            known_folder_path(&FOLDERID_PublicPictures)?,
            known_folder_path(&FOLDERID_PublicMusic)?,
            known_folder_path(&FOLDERID_PublicVideos)?,
        ],
    };
    Ok((system_root, destinations))
}

#[cfg(not(windows))]
pub fn restore_preserved_personal_files_for_current_user(
    _session_id: &str,
) -> Result<PersonalFileRestoreReport> {
    bail!("personal-file restoration is only available on Windows")
}

/// Builds the complete move plan and rejects data that cannot honestly be described as locally
/// preserved. No filesystem mutation occurs here.
pub fn plan_personal_file_preservation(
    target_root: &Path,
    session_id: &str,
) -> Result<PreservationPlan> {
    validate_target_root(target_root)?;
    validate_session_id(session_id)?;
    // PE is free to assign the offline Windows volume a letter other than C:. Shell Link tracking
    // may expose that current letter even when the link was authored against C:, so treat both as
    // the same old-system volume while keeping every other drive outside the deletion boundary.
    let target_drive = target_root.as_os_str().to_string_lossy().as_bytes()[0];

    let users = target_root.join("Users");
    if !users.is_dir() {
        bail!("the selected target does not contain an offline Users directory");
    }
    reject_reparse_or_remote_data(&users, true)?;

    let preserved_root = target_root.join(format!("LetRecovery_Preserved_{session_id}"));
    if preserved_root.exists() {
        bail!("the session preservation root already exists");
    }

    let mut profiles = std::fs::read_dir(&users)
        .with_context(|| format!("enumerate offline profiles under {}", users.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    profiles.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());

    let mut directories = Vec::new();
    let mut total_files = 0u64;
    let mut total_bytes = 0u64;
    let mut desktop_shortcuts_to_remove = Vec::new();
    let mut unresolved_desktop_shortcuts = 0u64;
    for profile in profiles {
        let profile_name = profile.file_name().to_string_lossy().into_owned();
        if is_excluded_profile(&profile_name) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(profile.path())
            .with_context(|| format!("inspect profile {}", profile.path().display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        reject_reparse_or_remote_data(&profile.path(), true)?;

        for directory_name in PERSONAL_DIRECTORY_NAMES {
            let source = profile.path().join(directory_name);
            let metadata = match std::fs::symlink_metadata(&source) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("inspect personal directory {}", source.display())
                    })
                }
            };
            if !metadata.is_dir() {
                bail!("personal path is not a directory: {}", source.display());
            }
            let (files, bytes) = inspect_local_tree(&source)?;
            if directory_name == "Desktop" {
                let (mut removable, unresolved) = classify_desktop_shortcuts(
                    &source,
                    &preserved_root.join(&profile_name).join(directory_name),
                    target_drive,
                )?;
                desktop_shortcuts_to_remove.append(&mut removable);
                unresolved_desktop_shortcuts = unresolved_desktop_shortcuts
                    .checked_add(unresolved)
                    .ok_or_else(|| anyhow!("desktop shortcut count overflow"))?;
            }
            total_files = total_files
                .checked_add(files)
                .ok_or_else(|| anyhow!("personal file count overflow"))?;
            total_bytes = total_bytes
                .checked_add(bytes)
                .ok_or_else(|| anyhow!("personal byte count overflow"))?;
            directories.push(PreservedDirectory {
                profile_name: profile_name.clone(),
                directory_name: directory_name.to_string(),
                source,
                destination: preserved_root.join(&profile_name).join(directory_name),
                files,
                bytes,
            });
        }
    }

    if directories.is_empty() {
        bail!("no supported local personal directories were found on the selected target");
    }

    Ok(PreservationPlan {
        target_root: target_root.to_path_buf(),
        preserved_root,
        directories,
        files: total_files,
        bytes: total_bytes,
        desktop_shortcuts_to_remove,
        unresolved_desktop_shortcuts,
    })
}

/// Moves all planned personal directories, then irreversibly removes exact old-system roots.
///
/// `on_irreversible` is called immediately before the first old-system delete. Before that point,
/// move failures are rolled back in reverse order. Once deletion begins, no old-system rollback is
/// claimed and the preservation root is intentionally retained.
pub fn execute_personal_file_preservation(
    plan: &PreservationPlan,
    mut on_irreversible: impl FnMut(),
) -> std::result::Result<PreservationReport, PreservationFailure> {
    let reversible = || PreservationFailure {
        stage: PreservationStage::Reversible,
        preserved_root: plan.preserved_root.clone(),
        error: anyhow!("personal-file preservation failed before old-system deletion"),
    };
    if plan.preserved_root.exists() {
        return Err(PreservationFailure {
            error: anyhow!("the session preservation root was created by another operation"),
            ..reversible()
        });
    }

    if let Err(error) = std::fs::create_dir(&plan.preserved_root)
        .with_context(|| format!("create {}", plan.preserved_root.display()))
    {
        return Err(PreservationFailure {
            error,
            ..reversible()
        });
    }

    let readme = plan.preserved_root.join("README.txt");
    let readme_body =
        "LetRecovery preserved local personal files here before reinstalling Windows.\r\n\
Only Desktop, Documents, Downloads, Pictures, Music and Videos were preserved.\r\n\
This directory is not a complete system backup.\r\n";
    if let Err(error) =
        std::fs::write(&readme, readme_body).with_context(|| format!("write {}", readme.display()))
    {
        let _ = std::fs::remove_dir(&plan.preserved_root);
        return Err(PreservationFailure {
            error,
            ..reversible()
        });
    }

    let mut moved = Vec::new();
    for directory in &plan.directories {
        let profile_destination = plan.preserved_root.join(&directory.profile_name);
        if let Err(error) = std::fs::create_dir_all(&profile_destination)
            .with_context(|| format!("create {}", profile_destination.display()))
            .and_then(|_| move_directory_same_volume(&directory.source, &directory.destination))
        {
            let rollback_error = rollback_moves(&moved);
            let rollback_incomplete = rollback_error.is_some();
            let cleanup_error = cleanup_empty_preservation_root(&plan.preserved_root);
            let mut detail = format!("move {}: {error:#}", directory.source.display());
            if let Some(rollback_error) = rollback_error {
                detail.push_str(&format!("; rollback failed: {rollback_error:#}"));
            }
            if let Some(cleanup_error) = cleanup_error {
                detail.push_str(&format!(
                    "; preservation-root cleanup failed: {cleanup_error:#}"
                ));
            }
            return Err(PreservationFailure {
                stage: if rollback_incomplete {
                    PreservationStage::RollbackIncomplete
                } else {
                    PreservationStage::Reversible
                },
                preserved_root: plan.preserved_root.clone(),
                error: anyhow!(detail),
            });
        }
        moved.push((directory.source.clone(), directory.destination.clone()));
    }

    on_irreversible();
    let mut deleted_roots = 0usize;
    let mut deleted_entries = 0u64;
    let mut deleted_desktop_shortcuts = 0u64;
    for shortcut in &plan.desktop_shortcuts_to_remove {
        if let Err(error) = fast_delete_path(shortcut, false) {
            return Err(PreservationFailure {
                stage: PreservationStage::OldSystemDeletionStarted,
                preserved_root: plan.preserved_root.clone(),
                error: error.context(format!(
                    "delete preserved Desktop shortcut {}",
                    shortcut.display()
                )),
            });
        }
        deleted_desktop_shortcuts = deleted_desktop_shortcuts.saturating_add(1);
    }
    for name in OLD_SYSTEM_DIRECTORIES {
        let path = plan.target_root.join(name);
        if !path.exists() {
            continue;
        }
        match fast_remove_tree(&path) {
            Ok(count) => {
                deleted_roots += 1;
                deleted_entries = deleted_entries.saturating_add(count);
            }
            Err(error) => {
                return Err(PreservationFailure {
                    stage: PreservationStage::OldSystemDeletionStarted,
                    preserved_root: plan.preserved_root.clone(),
                    error: error.context(format!("delete old-system directory {}", path.display())),
                })
            }
        }
    }
    for name in OLD_SYSTEM_FILES {
        let path = plan.target_root.join(name);
        if !path.exists() {
            continue;
        }
        if let Err(error) = fast_delete_path(&path, false) {
            return Err(PreservationFailure {
                stage: PreservationStage::OldSystemDeletionStarted,
                preserved_root: plan.preserved_root.clone(),
                error: error.context(format!("delete old-system file {}", path.display())),
            });
        }
        deleted_roots += 1;
        deleted_entries = deleted_entries.saturating_add(1);
    }

    Ok(PreservationReport {
        preserved_root: plan.preserved_root.clone(),
        preserved_directories: plan.directories.len(),
        preserved_files: plan.files,
        preserved_bytes: plan.bytes,
        deleted_roots,
        deleted_entries,
        deleted_desktop_shortcuts,
        unresolved_desktop_shortcuts: plan.unresolved_desktop_shortcuts,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShortcutTargetClass {
    OldSystemOutsideUsers,
    OldSystemUsers,
    Other,
}

fn classify_shortcut_target(target: &str, current_target_drive: u8) -> ShortcutTargetClass {
    let mut path = target.trim().replace('/', "\\");
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        path = rest.to_string();
    }
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !(bytes[0].eq_ignore_ascii_case(&b'C')
            || bytes[0].eq_ignore_ascii_case(&current_target_drive))
        || bytes[1] != b':'
        || bytes[2] != b'\\'
    {
        return ShortcutTargetClass::Other;
    }
    let mut components = Vec::new();
    for component in path[3..].split('\\') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            components.pop();
        } else {
            components.push(component);
        }
    }
    if components
        .first()
        .is_some_and(|component| component.eq_ignore_ascii_case("Users"))
    {
        ShortcutTargetClass::OldSystemUsers
    } else {
        ShortcutTargetClass::OldSystemOutsideUsers
    }
}

fn shortcut_targets_require_removal(
    com_target: Option<&str>,
    stored_local_target: Option<&str>,
    current_target_drive: u8,
) -> bool {
    match com_target.map(|target| classify_shortcut_target(target, current_target_drive)) {
        Some(ShortcutTargetClass::OldSystemOutsideUsers) => true,
        Some(ShortcutTargetClass::OldSystemUsers) => false,
        Some(ShortcutTargetClass::Other) | None => stored_local_target.is_some_and(|target| {
            classify_shortcut_target(target, current_target_drive)
                == ShortcutTargetClass::OldSystemOutsideUsers
        }),
    }
}

#[cfg(windows)]
fn classify_desktop_shortcuts(
    source: &Path,
    destination: &Path,
    current_target_drive: u8,
) -> Result<(Vec<PathBuf>, u64)> {
    let shortcuts = collect_desktop_shortcuts(source)?;
    if shortcuts.is_empty() {
        return Ok((Vec::new(), 0));
    }

    let apartment = match ShortcutComApartment::enter() {
        Ok(apartment) => Some(apartment),
        Err(error) => {
            log::warn!(
                "[PERSONAL FILES] Shell Link COM apartment unavailable; using the bounded MS-SHLLINK fallback: {error:#}"
            );
            None
        }
    };
    let mut removable = Vec::new();
    let mut unresolved = 0u64;
    let mut binary_fallbacks = 0u64;
    let mut com_retargeted_fallbacks = 0u64;
    for shortcut in shortcuts {
        let relative = shortcut
            .strip_prefix(source)
            .context("Desktop shortcut escaped its scanned root")?;
        let com_result = apartment
            .as_ref()
            .map(|_| read_shell_link_raw_target(&shortcut));
        let com_target = match &com_result {
            Some(Ok(Some(target))) => Some(target.as_str()),
            _ => None,
        };
        let com_class = com_target
            .map(|target| classify_shortcut_target(target, current_target_drive))
            .unwrap_or(ShortcutTargetClass::Other);
        let binary_result = if com_class == ShortcutTargetClass::Other {
            binary_fallbacks = binary_fallbacks.saturating_add(1);
            Some(read_shell_link_binary_local_target(&shortcut))
        } else {
            None
        };
        let stored_local_target = match &binary_result {
            Some(Ok(Some(target))) => Some(target.as_str()),
            _ => None,
        };
        if shortcut_targets_require_removal(com_target, stored_local_target, current_target_drive) {
            removable.push(destination.join(relative));
        } else if com_target.is_none()
            && matches!(binary_result, Some(Ok(None)) | Some(Err(_)) | None)
        {
            unresolved = unresolved.saturating_add(1);
        }
        if com_target.is_some_and(|target| {
            classify_shortcut_target(target, current_target_drive) == ShortcutTargetClass::Other
        }) && stored_local_target.is_some()
        {
            com_retargeted_fallbacks = com_retargeted_fallbacks.saturating_add(1);
        }
    }
    if binary_fallbacks != 0 {
        log::warn!(
            "[PERSONAL FILES] used the bounded MS-SHLLINK local-path fallback for {binary_fallbacks} Desktop shortcuts"
        );
    }
    if com_retargeted_fallbacks != 0 {
        log::warn!(
            "[PERSONAL FILES] Shell Link COM returned a non-target-volume path for {com_retargeted_fallbacks} shortcuts; classified their stored MS-SHLLINK local paths instead"
        );
    }
    Ok((removable, unresolved))
}

#[cfg(not(windows))]
fn classify_desktop_shortcuts(
    _source: &Path,
    _destination: &Path,
    _current_target_drive: u8,
) -> Result<(Vec<PathBuf>, u64)> {
    Ok((Vec::new(), 0))
}

#[cfg(windows)]
fn collect_desktop_shortcuts(root: &Path) -> Result<Vec<PathBuf>> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    let mut shortcuts = Vec::new();
    for entry in enumerate_directory(root)? {
        let path = root.join(&entry.name);
        if entry.attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            if entry.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0 {
                shortcuts.extend(collect_desktop_shortcuts(&path)?);
            }
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
        {
            shortcuts.push(path);
        }
    }
    Ok(shortcuts)
}

#[cfg(windows)]
struct ShortcutComApartment {
    uninitialize: bool,
}

#[cfg(windows)]
impl ShortcutComApartment {
    fn enter() -> Result<Self> {
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            return Ok(Self { uninitialize: true });
        }
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self {
                uninitialize: false,
            });
        }
        Err(anyhow!("CoInitializeEx failed: 0x{:08X}", result.0 as u32))
    }
}

#[cfg(windows)]
impl Drop for ShortcutComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(windows)]
fn read_shell_link_raw_target(shortcut: &Path) -> Result<Option<String>> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};

    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
        .context("CoCreateInstance(CLSID_ShellLink)")?;
    let persist: IPersistFile = link
        .cast()
        .context("IShellLinkW::QueryInterface(IPersistFile)")?;
    let shortcut_wide = wide_plain(shortcut);
    unsafe { persist.Load(windows::core::PCWSTR(shortcut_wide.as_ptr()), STGM_READ) }
        .with_context(|| format!("IPersistFile::Load({})", shortcut.display()))?;
    let mut target = [0u16; 260];
    unsafe { link.GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32) }
        .context("IShellLinkW::GetPath(SLGP_RAWPATH)")?;
    let length = target
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(target.len());
    if length == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf16_lossy(&target[..length])))
}

/// Reads only the local absolute-path representation defined by MS-SHLLINK section 2.3. This is
/// a deliberately bounded fallback for WinPE images where CLSID_ShellLink is not registered; it
/// does not interpret IDLists, networks, environment variables, property stores, or tracker data.
#[cfg(windows)]
fn read_shell_link_binary_local_target(shortcut: &Path) -> Result<Option<String>> {
    let metadata = std::fs::metadata(shortcut)
        .with_context(|| format!("inspect Shell Link fallback input {}", shortcut.display()))?;
    if metadata.len() > MAX_SHELL_LINK_BYTES {
        bail!("Shell Link exceeds the 1 MiB fallback parser limit");
    }
    let bytes = std::fs::read(shortcut)
        .with_context(|| format!("read Shell Link fallback input {}", shortcut.display()))?;
    parse_shell_link_binary_local_target(&bytes)
}

#[cfg(windows)]
fn parse_shell_link_binary_local_target(bytes: &[u8]) -> Result<Option<String>> {
    if bytes.len() < SHELL_LINK_HEADER_SIZE
        || read_link_u32(bytes, 0, "HeaderSize")? as usize != SHELL_LINK_HEADER_SIZE
        || bytes[4..20] != SHELL_LINK_CLSID
    {
        bail!("invalid MS-SHLLINK header");
    }
    let flags = read_link_u32(bytes, 20, "LinkFlags")?;
    let mut cursor = SHELL_LINK_HEADER_SIZE;
    if flags & 0x0000_0001 != 0 {
        let id_list_size = read_link_u16(bytes, cursor, "IDListSize")? as usize;
        cursor = cursor
            .checked_add(2)
            .and_then(|value| value.checked_add(id_list_size))
            .filter(|value| *value <= bytes.len())
            .ok_or_else(|| anyhow!("MS-SHLLINK IDList exceeds the file"))?;
    }
    // HasLinkInfo must be set and ForceNoLinkInfo must be clear. The fallback intentionally does
    // not guess a target from ItemIDs or optional ExtraData.
    if flags & 0x0000_0002 == 0 || flags & 0x0000_0100 != 0 {
        return Ok(None);
    }
    let link_info_size = read_link_u32(bytes, cursor, "LinkInfoSize")? as usize;
    let header_size = read_link_u32(bytes, cursor + 4, "LinkInfoHeaderSize")? as usize;
    if link_info_size < 0x1C || header_size < 0x1C || header_size > link_info_size {
        bail!("invalid MS-SHLLINK LinkInfo size");
    }
    let end = cursor
        .checked_add(link_info_size)
        .filter(|value| *value <= bytes.len())
        .ok_or_else(|| anyhow!("MS-SHLLINK LinkInfo exceeds the file"))?;
    let link_info_flags = read_link_u32(bytes, cursor + 8, "LinkInfoFlags")?;
    if link_info_flags & 0x0000_0001 == 0 {
        return Ok(None);
    }

    let ansi_base_offset = read_link_u32(bytes, cursor + 16, "LocalBasePathOffset")? as usize;
    let ansi_suffix_offset = read_link_u32(bytes, cursor + 24, "CommonPathSuffixOffset")? as usize;
    let unicode_offsets = if header_size >= 0x24 {
        Some((
            read_link_u32(bytes, cursor + 28, "LocalBasePathOffsetUnicode")? as usize,
            read_link_u32(bytes, cursor + 32, "CommonPathSuffixOffsetUnicode")? as usize,
        ))
    } else {
        None
    };

    let unicode_path = match unicode_offsets {
        Some((base, suffix)) if base != 0 && suffix != 0 => Some(combine_shell_link_path(
            &read_link_utf16(bytes, cursor, end, base)?,
            &read_link_utf16(bytes, cursor, end, suffix)?,
        )),
        _ => None,
    };
    if unicode_path.as_deref().is_some_and(|path| !path.is_empty()) {
        return Ok(unicode_path);
    }
    if ansi_base_offset == 0 || ansi_suffix_offset == 0 {
        return Ok(None);
    }
    let base = read_link_ascii(bytes, cursor, end, ansi_base_offset)?;
    let suffix = read_link_ascii(bytes, cursor, end, ansi_suffix_offset)?;
    if base.is_empty() {
        return Ok(None);
    }
    Ok(Some(combine_shell_link_path(&base, &suffix)))
}

#[cfg(windows)]
fn read_link_u16(bytes: &[u8], offset: usize, field: &str) -> Result<u16> {
    let value = bytes
        .get(offset..offset.saturating_add(2))
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| anyhow!("MS-SHLLINK {field} is truncated"))?;
    Ok(u16::from_le_bytes(value))
}

#[cfg(windows)]
fn read_link_u32(bytes: &[u8], offset: usize, field: &str) -> Result<u32> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| anyhow!("MS-SHLLINK {field} is truncated"))?;
    Ok(u32::from_le_bytes(value))
}

#[cfg(windows)]
fn read_link_utf16(bytes: &[u8], start: usize, end: usize, relative: usize) -> Result<String> {
    let offset = start
        .checked_add(relative)
        .filter(|value| *value < end && *value % 2 == 0)
        .ok_or_else(|| anyhow!("invalid MS-SHLLINK Unicode string offset"))?;
    let mut units = Vec::new();
    let mut cursor = offset;
    loop {
        if cursor.checked_add(2).is_none_or(|next| next > end) {
            bail!("unterminated MS-SHLLINK Unicode string");
        }
        let unit = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
        cursor += 2;
    }
    String::from_utf16(&units).context("invalid UTF-16 in MS-SHLLINK local path")
}

#[cfg(windows)]
fn read_link_ascii(bytes: &[u8], start: usize, end: usize, relative: usize) -> Result<String> {
    let offset = start
        .checked_add(relative)
        .filter(|value| *value < end)
        .ok_or_else(|| anyhow!("invalid MS-SHLLINK ANSI string offset"))?;
    let tail = &bytes[offset..end];
    let length = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| anyhow!("unterminated MS-SHLLINK ANSI string"))?;
    let value = &tail[..length];
    if !value.iter().all(u8::is_ascii) {
        bail!("non-ASCII MS-SHLLINK ANSI path is not classified by the fallback");
    }
    Ok(String::from_utf8(value.to_vec()).expect("ASCII is valid UTF-8"))
}

#[cfg(windows)]
fn combine_shell_link_path(base: &str, suffix: &str) -> String {
    if suffix.is_empty()
        || base
            .to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase())
    {
        base.to_string()
    } else {
        format!("{base}{suffix}")
    }
}

fn validate_target_root(path: &Path) -> Result<()> {
    let text = path.as_os_str().to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() != 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\'
    {
        bail!("personal-file preservation requires an exact DOS drive root");
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.len() != 32
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid preservation session id");
    }
    Ok(())
}

fn is_excluded_profile(name: &str) -> bool {
    EXCLUDED_PROFILE_NAMES
        .iter()
        .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn restore_preserved_personal_files_to(
    preserved_root: &Path,
    destinations: &RestoreKnownFolders,
) -> Result<PersonalFileRestoreReport> {
    reject_reparse_or_remote_data(preserved_root, true)
        .with_context(|| format!("inspect preservation root {}", preserved_root.display()))?;
    for destination in destinations
        .personal
        .iter()
        .chain(destinations.public.iter())
    {
        if !destination.is_absolute() {
            bail!(
                "known-folder destination is not absolute: {}",
                destination.display()
            );
        }
        std::fs::create_dir_all(destination)
            .with_context(|| format!("create known folder {}", destination.display()))?;
    }

    let mut profiles = Vec::new();
    for entry in enumerate_restore_directory(preserved_root)? {
        let name = entry.file_name();
        if name.to_string_lossy().eq_ignore_ascii_case("README.txt") {
            if !entry.file_type()?.is_file() {
                bail!("preservation README is not a regular file");
            }
            continue;
        }
        if !entry.file_type()?.is_dir() {
            bail!(
                "unexpected object in preservation root: {}",
                entry.path().display()
            );
        }
        let profile_name = name
            .to_str()
            .ok_or_else(|| anyhow!("preserved profile name is not valid Unicode"))?
            .to_string();
        profiles.push((profile_name, entry.path()));
    }
    profiles.sort_by(|left, right| {
        left.0
            .to_ascii_lowercase()
            .cmp(&right.0.to_ascii_lowercase())
    });
    if profiles.is_empty() {
        bail!("preservation root contains no profile directories");
    }

    let mut counters = RestoreCounters::default();
    for (profile_name, profile_root) in &profiles {
        reject_reparse_or_remote_data(profile_root, true)?;
        let target_set = if profile_name.eq_ignore_ascii_case("Public") {
            &destinations.public
        } else {
            &destinations.personal
        };
        for entry in enumerate_restore_directory(profile_root)? {
            let directory_name = entry
                .file_name()
                .to_str()
                .ok_or_else(|| anyhow!("preserved known-folder name is not valid Unicode"))?
                .to_string();
            let Some(index) = PERSONAL_DIRECTORY_NAMES
                .iter()
                .position(|expected| directory_name.eq_ignore_ascii_case(expected))
            else {
                bail!(
                    "unexpected directory in preserved profile: {}",
                    entry.path().display()
                );
            };
            reject_reparse_or_remote_data(&entry.path(), true).with_context(|| {
                format!(
                    "preserved known-folder root is not a regular local directory: {}",
                    entry.path().display()
                )
            })?;
            merge_preserved_directory(
                &entry.path(),
                &target_set[index],
                profile_name,
                &mut counters,
            )?;
            remove_restored_empty_directory(&entry.path()).with_context(|| {
                format!(
                    "remove restored known-folder root {}",
                    entry.path().display()
                )
            })?;
        }
        remove_restored_empty_directory(profile_root)
            .with_context(|| format!("remove restored profile root {}", profile_root.display()))?;
    }

    let readme = preserved_root.join("README.txt");
    match std::fs::symlink_metadata(&readme) {
        Ok(metadata) => {
            if metadata.is_dir() && !metadata_is_reparse_point(&metadata) {
                bail!("preservation README unexpectedly became a directory");
            }
            fast_delete_path(&readme, metadata.is_dir())
                .with_context(|| format!("remove {}", readme.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", readme.display())),
    }
    remove_restored_empty_directory(preserved_root)
        .with_context(|| format!("remove restored root {}", preserved_root.display()))?;

    Ok(PersonalFileRestoreReport {
        preserved_root: preserved_root.to_path_buf(),
        current_profile_root: destinations.current_profile_root.clone(),
        personal_directories: destinations.personal.clone(),
        public_directories: destinations.public.clone(),
        source_profiles: profiles.len(),
        restored_directories: counters.directories,
        restored_files: counters.files,
        renamed_conflicts: counters.renamed_conflicts,
    })
}

fn enumerate_restore_directory(directory: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut entries = std::fs::read_dir(directory)
        .with_context(|| format!("enumerate {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.file_name().to_string_lossy().to_ascii_lowercase())
    });
    Ok(entries)
}

fn remove_restored_empty_directory(directory: &Path) -> Result<()> {
    let remaining = enumerate_restore_directory(directory)?;
    if !remaining.is_empty() {
        bail!(
            "refusing to remove non-empty restored directory {}",
            directory.display()
        );
    }
    // Preserved Known Folders retain their original directory attributes. Windows commonly marks
    // folders such as Desktop read-only/system, for which std::fs::remove_dir returns access
    // denied even after every child has been restored. Reuse the already audited handle-based
    // deletion boundary: it opens the directory itself (never a reparse target), asks Windows to
    // ignore READONLY where supported, and has the documented Win7 FileDispositionInfo fallback.
    fast_delete_path(directory, true)?;
    match std::fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!(
            "restored directory still exists after deletion: {}",
            directory.display()
        ),
        Err(error) => Err(error)
            .with_context(|| format!("verify restored directory deletion {}", directory.display())),
    }
}

fn merge_preserved_directory(
    source: &Path,
    destination: &Path,
    source_profile: &str,
    counters: &mut RestoreCounters,
) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create restore destination {}", destination.display()))?;
    counters.directories = counters
        .directories
        .checked_add(1)
        .ok_or_else(|| anyhow!("restored directory count overflow"))?;
    for entry in enumerate_restore_directory(source)? {
        let source_path = entry.path();
        let source_metadata = std::fs::symlink_metadata(&source_path)?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("desktop.ini"))
        {
            if !source_metadata.is_file() || metadata_is_reparse_point(&source_metadata) {
                bail!(
                    "ignored Windows known-folder metadata is not a regular file: {}",
                    source_path.display()
                );
            }
            // desktop.ini describes the old Known Folder's presentation and localized display
            // name. Copying it into the newly created profile is neither user-data restoration nor
            // desirable: it collides with Windows' new metadata and becomes a visible
            // `desktop (from ...).ini`. Delete only this exact ordinary source file and let Windows
            // own the destination Known Folder metadata.
            fast_delete_path(&source_path, false).with_context(|| {
                format!(
                    "discard obsolete Windows known-folder metadata {}",
                    source_path.display()
                )
            })?;
            continue;
        }
        let source_is_reparse = metadata_is_reparse_point(&source_metadata);
        let mut destination_path = destination.join(entry.file_name());
        let destination_metadata = match std::fs::symlink_metadata(&destination_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect restore destination {}", destination_path.display())
                })
            }
        };
        if source_metadata.is_dir()
            && !source_is_reparse
            && destination_metadata
                .as_ref()
                .is_some_and(std::fs::Metadata::is_dir)
            && !destination_metadata
                .as_ref()
                .is_some_and(metadata_is_reparse_point)
        {
            merge_preserved_directory(&source_path, &destination_path, source_profile, counters)?;
            remove_restored_empty_directory(&source_path)
                .with_context(|| format!("remove merged directory {}", source_path.display()))?;
            continue;
        }
        if destination_metadata.is_some() {
            destination_path =
                unique_conflict_destination(destination, &entry.file_name(), source_profile)?;
            counters.renamed_conflicts = counters
                .renamed_conflicts
                .checked_add(1)
                .ok_or_else(|| anyhow!("restore conflict count overflow"))?;
        }
        if source_metadata.is_dir() && !source_is_reparse {
            merge_preserved_directory(&source_path, &destination_path, source_profile, counters)?;
            remove_restored_empty_directory(&source_path).with_context(|| {
                format!("remove restored source directory {}", source_path.display())
            })?;
        } else {
            move_restore_entry(&source_path, &destination_path)?;
            counters.files = counters
                .files
                .checked_add(1)
                .ok_or_else(|| anyhow!("restored file count overflow"))?;
        }
    }
    Ok(())
}

fn unique_conflict_destination(
    parent: &Path,
    original_name: &std::ffi::OsStr,
    source_profile: &str,
) -> Result<PathBuf> {
    let original = Path::new(original_name);
    let stem = original
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("conflicting preserved name is not valid Unicode"))?;
    let extension = original.extension().and_then(|value| value.to_str());
    let safe_profile: String = source_profile
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ => character,
        })
        .collect();
    for ordinal in 1..=10_000u32 {
        let suffix = if ordinal == 1 {
            format!(" (from {safe_profile})")
        } else {
            format!(" (from {safe_profile} {ordinal})")
        };
        let name = match extension {
            Some(extension) => format!("{stem}{suffix}.{extension}"),
            None => format!("{stem}{suffix}"),
        };
        let candidate = parent.join(name);
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("unable to allocate a bounded conflict name")
}

#[cfg(windows)]
fn move_restore_entry(source: &Path, destination: &Path) -> Result<()> {
    use std::io::Write as _;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let source_metadata = std::fs::symlink_metadata(source)
        .with_context(|| format!("inspect restore source {}", source.display()))?;
    let source_wide = wide(source);
    let destination_wide = wide(destination);

    if metadata_is_reparse_point(&source_metadata) {
        // Compatibility junctions and symbolic links are preserved as leaf objects. CopyFileExW
        // without COPY_FILE_COPY_SYMLINK follows a link, while that flag does not support every
        // directory reparse-point form. A same-volume rename retains the link itself and never
        // opens its target.
        unsafe {
            MoveFileExW(
                windows::core::PCWSTR(source_wide.as_ptr()),
                windows::core::PCWSTR(destination_wide.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .with_context(|| {
            format!(
                "restore reparse leaf {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
    } else {
        // Microsoft documents that Windows 8+ CopyFileExW copies the source file's security
        // resource properties. That includes the owner from the old profile SID, so resetting only
        // the DACL still leaves the restored object bound to an obsolete account identity.
        // OpenOptions::create_new creates a genuinely new
        // destination object with the current token as owner and the Known Folder parent's
        // inherited DACL on every supported Windows version. It also preserves the caller's
        // no-overwrite conflict policy without requiring WRITE_OWNER or a second security rewrite.
        let mut source_file = std::fs::File::open(source)
            .with_context(|| format!("open restore source {}", source.display()))?;
        let opened_source_metadata = source_file
            .metadata()
            .with_context(|| format!("inspect opened restore source {}", source.display()))?;
        if !opened_source_metadata.is_file()
            || opened_source_metadata.len() != source_metadata.len()
        {
            bail!("opened restore source did not match the inspected regular file");
        }
        let mut destination_file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
        {
            Ok(file) => file,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "create restore destination {} for {}",
                        destination.display(),
                        source.display()
                    )
                })
            }
        };
        let copy_result = (|| -> Result<()> {
            let copied =
                std::io::copy(&mut source_file, &mut destination_file).with_context(|| {
                    format!(
                        "stream restore contents {} -> {}",
                        source.display(),
                        destination.display()
                    )
                })?;
            if copied != source_metadata.len() {
                bail!(
                    "streamed restore length mismatch: expected {} bytes, wrote {copied}",
                    source_metadata.len()
                );
            }
            destination_file
                .flush()
                .with_context(|| format!("flush restore destination {}", destination.display()))?;
            destination_file
                .sync_all()
                .with_context(|| format!("sync restore destination {}", destination.display()))?;
            Ok(())
        })();
        drop(destination_file);
        if let Err(copy_error) = copy_result {
            let cleanup_error = fast_delete_path(destination, false).err();
            return match cleanup_error {
                Some(cleanup_error) => Err(anyhow!(
                    "stream restore failed: {copy_error:#}; removing the uncommitted destination also failed: {cleanup_error:#}; preserved source remains at {}",
                    source.display()
                )),
                None => Err(copy_error).with_context(|| {
                    format!(
                        "stream restore to {} failed; uncommitted destination removed and source preserved",
                        destination.display()
                    )
                }),
            };
        }

        let destination_metadata = std::fs::symlink_metadata(destination).with_context(|| {
            format!("read back copied restore target {}", destination.display())
        })?;
        if destination_metadata.is_dir()
            || metadata_is_reparse_point(&destination_metadata)
            || destination_metadata.len() != source_metadata.len()
        {
            bail!("copied restore target did not match the source file type and length");
        }
        fast_delete_path(source, false)
            .with_context(|| format!("remove streamed restore source {}", source.display()))?;
    }

    if std::fs::symlink_metadata(source).is_ok() || std::fs::symlink_metadata(destination).is_err()
    {
        bail!("restore did not reach its authoritative source-absent/target-present postcondition")
    }
    Ok(())
}

#[cfg(not(windows))]
fn move_restore_entry(source: &Path, destination: &Path) -> Result<()> {
    std::fs::rename(source, destination)
        .with_context(|| format!("restore {} -> {}", source.display(), destination.display()))
}

#[cfg(windows)]
fn known_folder_path(id: &windows::core::GUID) -> Result<PathBuf> {
    use std::ffi::c_void;
    use windows::core::{w, PCSTR};
    use windows::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_CREATE};

    type CoTaskMemFreeFn = unsafe extern "system" fn(*const c_void);
    struct LoadedLibrary(HMODULE);
    impl Drop for LoadedLibrary {
        fn drop(&mut self) {
            unsafe {
                let _ = FreeLibrary(self.0);
            }
        }
    }

    // New SDK import libraries can redirect this legacy symbol through combase.dll, which is
    // absent on an unmodified Windows 7 installation. ole32.dll is a Windows known DLL and has
    // exported CoTaskMemFree throughout the supported range, so resolve that documented export.
    let library =
        LoadedLibrary(unsafe { LoadLibraryW(w!("ole32.dll")) }.context("LoadLibraryW(ole32.dll)")?);
    let address = unsafe { GetProcAddress(library.0, PCSTR(c"CoTaskMemFree".as_ptr().cast())) }
        .ok_or_else(windows::core::Error::from_win32)
        .context("GetProcAddress(CoTaskMemFree)")?;
    let free: CoTaskMemFreeFn = unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, CoTaskMemFreeFn>(address)
    };
    let pointer = unsafe { SHGetKnownFolderPath(id, KF_FLAG_CREATE, None) }
        .context("SHGetKnownFolderPath(current user)")?;
    struct Guard {
        pointer: *mut u16,
        free: CoTaskMemFreeFn,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            if !self.pointer.is_null() {
                unsafe { (self.free)(self.pointer.cast_const().cast()) };
            }
        }
    }
    let _guard = Guard {
        pointer: pointer.0,
        free,
    };
    let value = unsafe { pointer.to_string() }.context("decode known-folder path")?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        bail!("SHGetKnownFolderPath returned a non-absolute path");
    }
    Ok(path)
}

fn cleanup_empty_preservation_root(root: &Path) -> Option<anyhow::Error> {
    let readme = root.join("README.txt");
    let _ = std::fs::remove_file(readme);
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let _ = std::fs::remove_dir(entry.path());
            }
        }
    }
    std::fs::remove_dir(root).err().map(anyhow::Error::from)
}

fn rollback_moves(moved: &[(PathBuf, PathBuf)]) -> Option<anyhow::Error> {
    let mut failures = Vec::new();
    for (source, destination) in moved.iter().rev() {
        if let Err(error) = move_directory_same_volume(destination, source) {
            failures.push(format!(
                "{} -> {}: {error:#}",
                destination.display(),
                source.display()
            ));
        }
    }
    (!failures.is_empty()).then(|| anyhow!(failures.join("; ")))
}

#[cfg(windows)]
fn move_directory_same_volume(source: &Path, destination: &Path) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let source = wide(source);
    let destination = wide(destination);
    // Microsoft documents directory moves as same-drive, destination-must-not-exist metadata
    // operations. WRITE_THROUGH makes the successful rename durable before deletion begins.
    unsafe {
        MoveFileExW(
            windows::core::PCWSTR(source.as_ptr()),
            windows::core::PCWSTR(destination.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .context("MoveFileExW(MOVEFILE_WRITE_THROUGH)")
}

#[cfg(not(windows))]
fn move_directory_same_volume(source: &Path, destination: &Path) -> Result<()> {
    std::fs::rename(source, destination).context("same-volume directory rename")
}

#[cfg(windows)]
fn inspect_local_tree(root: &Path) -> Result<(u64, u64)> {
    inspect_local_tree_windows(root)
}

#[cfg(not(windows))]
fn inspect_local_tree(root: &Path) -> Result<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            files = files.saturating_add(1);
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok((files, bytes))
}

#[cfg(windows)]
fn inspect_local_tree_windows(root: &Path) -> Result<(u64, u64)> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_OFFLINE,
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
        FILE_ATTRIBUTE_REPARSE_POINT,
    };
    reject_reparse_or_remote_data(root, true)?;
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in enumerate_directory(root)? {
        let attrs = entry.attributes;
        let path = root.join(&entry.name);
        // Legacy profile folders routinely contain compatibility junctions such as
        // `Documents\My Music`. Moving the selected top-level directory does not traverse these
        // links, and the later old-system deletion also treats them as leaf objects. Exclude the
        // linked target from the byte count instead of rejecting every normal Windows profile.
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            continue;
        }
        let unsafe_attrs = FILE_ATTRIBUTE_ENCRYPTED.0
            | FILE_ATTRIBUTE_OFFLINE.0
            | FILE_ATTRIBUTE_RECALL_ON_OPEN.0
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0;
        if attrs & unsafe_attrs != 0 {
            bail!(
                "personal data is encrypted, offline or recall-on-access: {}",
                path.display()
            );
        }
        if attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            let (child_files, child_bytes) = inspect_local_tree_windows(&path)?;
            files = files
                .checked_add(child_files)
                .ok_or_else(|| anyhow!("personal file count overflow"))?;
            bytes = bytes
                .checked_add(child_bytes)
                .ok_or_else(|| anyhow!("personal byte count overflow"))?;
        } else {
            files = files
                .checked_add(1)
                .ok_or_else(|| anyhow!("personal file count overflow"))?;
            bytes = bytes
                .checked_add(entry.bytes)
                .ok_or_else(|| anyhow!("personal byte count overflow"))?;
        }
    }
    Ok((files, bytes))
}

#[cfg(windows)]
fn reject_reparse_or_remote_data(path: &Path, allow_directory: bool) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED,
        FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    let path_wide = wide(path);
    let attrs = unsafe { GetFileAttributesW(windows::core::PCWSTR(path_wide.as_ptr())) };
    if attrs == u32::MAX {
        return Err(windows::core::Error::from_win32())
            .with_context(|| format!("GetFileAttributesW({})", path.display()));
    }
    if allow_directory && attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        bail!("expected a directory: {}", path.display());
    }
    let unsafe_attrs = FILE_ATTRIBUTE_REPARSE_POINT.0
        | FILE_ATTRIBUTE_ENCRYPTED.0
        | FILE_ATTRIBUTE_OFFLINE.0
        | FILE_ATTRIBUTE_RECALL_ON_OPEN.0
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0;
    if attrs & unsafe_attrs != 0 {
        bail!(
            "path is reparse, encrypted, offline or recall-on-access: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn reject_reparse_or_remote_data(path: &Path, allow_directory: bool) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || (allow_directory && !metadata.is_dir()) {
        bail!("unsupported personal path: {}", path.display());
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Debug)]
struct DirectoryEntry {
    name: OsString,
    attributes: u32,
    bytes: u64,
}

#[cfg(windows)]
fn enumerate_directory(directory: &Path) -> Result<Vec<DirectoryEntry>> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
    };

    struct FindGuard(HANDLE);
    impl Drop for FindGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = FindClose(self.0);
            }
        }
    }

    let pattern = directory.join("*");
    let pattern = wide(&pattern);
    let mut data = WIN32_FIND_DATAW::default();
    // LARGE_FETCH is supported from Windows 7/Server 2008 R2 and reduces directory-query round
    // trips. FindExInfoBasic omits the unused short name and the API returns entries unsorted.
    let handle = unsafe {
        FindFirstFileExW(
            windows::core::PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    }
    .with_context(|| format!("FindFirstFileExW({})", directory.display()))?;
    if handle == INVALID_HANDLE_VALUE {
        bail!("FindFirstFileExW returned an invalid handle");
    }
    let _guard = FindGuard(handle);
    let mut entries = Vec::new();
    loop {
        let name_len = data
            .cFileName
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(data.cFileName.len());
        let name = OsString::from_wide(&data.cFileName[..name_len]);
        if name != "." && name != ".." {
            entries.push(DirectoryEntry {
                name,
                attributes: data.dwFileAttributes,
                bytes: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
            });
        }
        data = WIN32_FIND_DATAW::default();
        match unsafe { FindNextFileW(handle, &mut data) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("FindNextFileW({})", directory.display()))
            }
        }
    }
    Ok(entries)
}

#[cfg(windows)]
fn fast_remove_tree(root: &Path) -> Result<u64> {
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    let root_wide = wide(root);
    let attrs = unsafe { GetFileAttributesW(windows::core::PCWSTR(root_wide.as_ptr())) };
    if attrs == u32::MAX {
        return Err(windows::core::Error::from_win32())
            .with_context(|| format!("GetFileAttributesW({})", root.display()));
    }
    if attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        fast_delete_path(root, false)?;
        return Ok(1);
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        fast_delete_path(root, true)?;
        return Ok(1);
    }

    let mut deleted = 0u64;
    for entry in enumerate_directory(root)? {
        let path = root.join(&entry.name);
        if entry.attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
            && entry.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
        {
            deleted = deleted.saturating_add(fast_remove_tree(&path)?);
        } else {
            fast_delete_path(&path, entry.attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0)?;
            deleted = deleted.saturating_add(1);
        }
    }
    fast_delete_path(root, true)?;
    Ok(deleted.saturating_add(1))
}

#[cfg(not(windows))]
fn fast_remove_tree(root: &Path) -> Result<u64> {
    let count = walkdir::WalkDir::new(root).into_iter().count() as u64;
    std::fs::remove_dir_all(root)?;
    Ok(count)
}

#[cfg(windows)]
fn fast_delete_path(path: &Path, directory: bool) -> Result<()> {
    use windows::Win32::Foundation::{
        CloseHandle, BOOLEAN, ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx,
        GetFileInformationByHandleEx, SetFileInformationByHandle, DELETE, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, OPEN_EXISTING,
    };

    struct HandleGuard(windows::Win32::Foundation::HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    let path_wide = wide(path);
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    let handle = HandleGuard(
        unsafe {
            CreateFileW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                DELETE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                flags,
                None,
            )
        }
        .with_context(|| format!("open for deletion {}", path.display()))?,
    );

    let extended = FILE_DISPOSITION_INFO_EX {
        Flags: windows::Win32::Storage::FileSystem::FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0,
        ),
    };
    let extended_result = unsafe {
        SetFileInformationByHandle(
            handle.0,
            FileDispositionInfoEx,
            (&extended as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    let result = match extended_result {
        Ok(()) => Ok(()),
        Err(error)
            if [
                ERROR_INVALID_FUNCTION.to_hresult(),
                ERROR_NOT_SUPPORTED.to_hresult(),
                ERROR_INVALID_PARAMETER.to_hresult(),
            ]
            .contains(&error.code()) =>
        {
            // FileDispositionInfoEx is not supported before Windows 8 and individual file systems
            // may reject POSIX semantics. Windows Vista/7 support basic FileDispositionInfo.
            let basic = FILE_DISPOSITION_INFO {
                DeleteFile: BOOLEAN(1),
            };
            let basic_result = unsafe {
                SetFileInformationByHandle(
                    handle.0,
                    FileDispositionInfo,
                    (&basic as *const FILE_DISPOSITION_INFO).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            };
            match basic_result {
                Ok(()) => return Ok(()),
                Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => {}
                Err(error) => return Err(error.into()),
            }
            // Reopen the same object (including a reparse point itself) with attribute access so
            // the Win7 path never follows a link through path-based SetFileAttributesW.
            let fallback_handle = HandleGuard(
                unsafe {
                    CreateFileW(
                        windows::core::PCWSTR(path_wide.as_ptr()),
                        DELETE.0 | FILE_READ_ATTRIBUTES.0 | FILE_WRITE_ATTRIBUTES.0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        None,
                        OPEN_EXISTING,
                        flags,
                        None,
                    )
                }
                .with_context(|| format!("reopen for Win7 deletion {}", path.display()))?,
            );
            let mut attributes = FILE_BASIC_INFO::default();
            let query_result = unsafe {
                GetFileInformationByHandleEx(
                    fallback_handle.0,
                    FileBasicInfo,
                    (&mut attributes as *mut FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            };
            if query_result.is_ok() && attributes.FileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
                attributes.FileAttributes &= !FILE_ATTRIBUTE_READONLY.0;
                if attributes.FileAttributes == 0 {
                    attributes.FileAttributes = FILE_ATTRIBUTE_NORMAL.0;
                }
                let set_result = unsafe {
                    SetFileInformationByHandle(
                        fallback_handle.0,
                        FileBasicInfo,
                        (&attributes as *const FILE_BASIC_INFO).cast(),
                        std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                    )
                };
                if let Err(error) = set_result {
                    return Err(error).context("clear read-only attribute on the opened object");
                }
            } else if let Err(error) = query_result {
                return Err(error).context("query opened object attributes before Win7 deletion");
            }
            unsafe {
                SetFileInformationByHandle(
                    fallback_handle.0,
                    FileDispositionInfo,
                    (&basic as *const FILE_DISPOSITION_INFO).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            }
        }
        Err(error) => Err(error),
    };
    result.with_context(|| format!("mark for deletion {}", path.display()))
}

#[cfg(not(windows))]
fn fast_delete_path(path: &Path, directory: bool) -> Result<()> {
    if directory {
        std::fs::remove_dir(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let text = path.as_os_str().to_string_lossy();
    let extended = if text.starts_with(r"\\?\") {
        text.into_owned()
    } else if let Some(rest) = text.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{text}")
    };
    OsString::from(extended)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn wide_plain(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x00000400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "letrecovery-personal-files-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn fixed_personal_folder_set_is_narrow() {
        assert_eq!(
            PERSONAL_DIRECTORY_NAMES,
            [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Music",
                "Videos"
            ]
        );
        assert!(!PERSONAL_DIRECTORY_NAMES.contains(&"AppData"));
    }

    #[test]
    fn excluded_profiles_do_not_hide_public_data() {
        assert!(is_excluded_profile("Default"));
        assert!(is_excluded_profile("defaultuser0"));
        assert!(!is_excluded_profile("Public"));
        assert!(!is_excluded_profile("Alice"));
    }

    #[test]
    fn session_id_is_exact_lower_hex() {
        assert!(validate_session_id("0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_session_id("0123456789ABCDEF0123456789ABCDEF").is_err());
        assert!(validate_session_id("short").is_err());
    }

    #[test]
    fn rollback_moves_restores_every_directory_in_reverse_order() {
        let root = temp_root("rollback");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let destination_a = root.join("destination-a");
        let destination_b = root.join("destination-b");
        std::fs::create_dir(&source_a).unwrap();
        std::fs::create_dir(&source_b).unwrap();
        move_directory_same_volume(&source_a, &destination_a).unwrap();
        move_directory_same_volume(&source_b, &destination_b).unwrap();
        assert!(rollback_moves(&[
            (source_a.clone(), destination_a.clone()),
            (source_b.clone(), destination_b.clone()),
        ])
        .is_none());
        assert!(source_a.is_dir());
        assert!(source_b.is_dir());
        assert!(!destination_a.exists());
        assert!(!destination_b.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fast_delete_removes_nested_tree_without_following_reparse_points() {
        let root = temp_root("delete");
        let old = root.join("Windows");
        std::fs::create_dir_all(old.join("System32").join("drivers")).unwrap();
        std::fs::write(old.join("System32").join("kernel.bin"), b"kernel").unwrap();
        std::fs::write(
            old.join("System32").join("drivers").join("disk.sys"),
            b"driver",
        )
        .unwrap();
        let deleted = fast_remove_tree(&old).unwrap();
        assert!(deleted >= 5);
        assert!(!old.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_system_allowlist_does_not_include_preservation_root_or_unknown_data() {
        let names = OLD_SYSTEM_DIRECTORIES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert!(names.contains("Windows"));
        assert!(names.contains("Users"));
        assert!(!names.contains("LetRecovery_Preserved_session"));
        assert!(!names.contains("Data"));
    }

    #[test]
    fn desktop_shortcut_target_filter_tracks_the_authenticated_offline_drive() {
        for target in [
            r"C:\Windows\System32\cmd.exe",
            r"c:/Program Files/App/app.exe",
            r"\\?\C:\ProgramData\tool.exe",
            r"C:\Users\Alice\..\..\Windows\explorer.exe",
            r"C:\UsersEvil\tool.exe",
            r"C:\",
            r"D:\Windows\System32\notepad.exe",
        ] {
            assert_eq!(
                classify_shortcut_target(target, b'D'),
                ShortcutTargetClass::OldSystemOutsideUsers,
                "{target}"
            );
        }
        for target in [
            r"C:\Users\Alice\document.txt",
            r"c:\users\Public\shared.txt",
            r"D:\Users\Alice\document.txt",
            r"E:\Program Files\App\app.exe",
            r"%windir%\System32\cmd.exe",
            r"C:relative\tool.exe",
            r"\\server\share\tool.exe",
            "",
        ] {
            assert!(
                classify_shortcut_target(target, b'D')
                    != ShortcutTargetClass::OldSystemOutsideUsers,
                "{target}"
            );
        }
        assert!(shortcut_targets_require_removal(
            Some(r"X:\Windows\System32\notepad.exe"),
            Some(r"C:\Windows\System32\notepad.exe"),
            b'D'
        ));
        assert!(!shortcut_targets_require_removal(
            Some(r"D:\Users\Alice\document.txt"),
            Some(r"C:\Windows\System32\notepad.exe"),
            b'D'
        ));
    }

    #[cfg(windows)]
    #[test]
    fn shell_link_contract_returns_the_raw_target_without_resolving_it() {
        use windows::core::Interface;
        use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

        let root = temp_root("shell-link");
        let shortcut = root.join("legacy.lnk");
        let _apartment = ShortcutComApartment::enter().unwrap();
        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.unwrap();
        let target = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("notepad.exe")
            .to_string_lossy()
            .into_owned();
        let target_wide = target
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        unsafe { link.SetPath(windows::core::PCWSTR(target_wide.as_ptr())) }.unwrap();
        let persist: IPersistFile = link.cast().unwrap();
        let shortcut_wide = wide_plain(&shortcut);
        unsafe { persist.Save(windows::core::PCWSTR(shortcut_wide.as_ptr()), true) }.unwrap();

        let com_target = read_shell_link_raw_target(&shortcut)
            .unwrap()
            .expect("existing local target must resolve through COM");
        assert!(com_target.eq_ignore_ascii_case(&target));
        let binary_target = read_shell_link_binary_local_target(&shortcut)
            .unwrap()
            .expect("existing local target must have LinkInfo");
        assert!(binary_target.eq_ignore_ascii_case(&target));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn delete_api_failure_after_the_boundary_keeps_preserved_files_and_reports_partial_state() {
        use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
        use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, OPEN_EXISTING};

        let root = temp_root("delete-failure");
        let desktop = root.join("Users").join("Alice").join("Desktop");
        let old_system = root.join("Windows").join("System32");
        std::fs::create_dir_all(&desktop).unwrap();
        std::fs::create_dir_all(&old_system).unwrap();
        std::fs::write(desktop.join("keep.txt"), b"keep").unwrap();
        let locked_file = old_system.join("locked.dll");
        std::fs::write(&locked_file, b"locked").unwrap();
        let locked_wide = wide(&locked_file);
        let handle = unsafe {
            CreateFileW(
                windows::core::PCWSTR(locked_wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        }
        .unwrap();
        let preserved_root = root.join("LetRecovery_Preserved_test");
        let plan = PreservationPlan {
            target_root: root.clone(),
            preserved_root: preserved_root.clone(),
            directories: vec![PreservedDirectory {
                profile_name: "Alice".into(),
                directory_name: "Desktop".into(),
                source: desktop,
                destination: preserved_root.join("Alice").join("Desktop"),
                files: 1,
                bytes: 4,
            }],
            files: 1,
            bytes: 4,
            desktop_shortcuts_to_remove: Vec::new(),
            unresolved_desktop_shortcuts: 0,
        };
        let irreversible = std::cell::Cell::new(false);

        let error = execute_personal_file_preservation(&plan, || irreversible.set(true))
            .expect_err("the non-delete-sharing handle must block old-system deletion");

        assert!(irreversible.get());
        assert_eq!(error.stage, PreservationStage::OldSystemDeletionStarted);
        assert!(preserved_root
            .join("Alice")
            .join("Desktop")
            .join("keep.txt")
            .is_file());
        assert!(locked_file.is_file());
        unsafe {
            let _ = CloseHandle(handle);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execute_moves_personal_directories_really_deletes_old_system_and_keeps_unknown_data() {
        let root = temp_root("execute");
        let desktop = root.join("Users").join("Alice").join("Desktop");
        let downloads = root.join("Users").join("Alice").join("Downloads");
        std::fs::create_dir_all(&desktop).unwrap();
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(desktop.join("desktop.txt"), b"desktop").unwrap();
        std::fs::write(downloads.join("download.bin"), b"download").unwrap();
        std::fs::create_dir_all(root.join("Windows").join("System32")).unwrap();
        std::fs::write(
            root.join("Windows").join("System32").join("old.dll"),
            b"old",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("Data")).unwrap();
        std::fs::write(root.join("Data").join("keep.txt"), b"keep").unwrap();
        let preserved_root = root.join("LetRecovery_Preserved_test");
        let plan = PreservationPlan {
            target_root: root.clone(),
            preserved_root: preserved_root.clone(),
            directories: vec![
                PreservedDirectory {
                    profile_name: "Alice".into(),
                    directory_name: "Desktop".into(),
                    source: desktop,
                    destination: preserved_root.join("Alice").join("Desktop"),
                    files: 1,
                    bytes: 7,
                },
                PreservedDirectory {
                    profile_name: "Alice".into(),
                    directory_name: "Downloads".into(),
                    source: downloads,
                    destination: preserved_root.join("Alice").join("Downloads"),
                    files: 1,
                    bytes: 8,
                },
            ],
            files: 2,
            bytes: 15,
            desktop_shortcuts_to_remove: Vec::new(),
            unresolved_desktop_shortcuts: 0,
        };
        let irreversible = std::cell::Cell::new(false);

        let report = execute_personal_file_preservation(&plan, || irreversible.set(true)).unwrap();

        assert!(irreversible.get());
        assert_eq!(report.preserved_directories, 2);
        assert!(preserved_root
            .join("Alice")
            .join("Desktop")
            .join("desktop.txt")
            .is_file());
        assert!(preserved_root
            .join("Alice")
            .join("Downloads")
            .join("download.bin")
            .is_file());
        assert!(!root.join("Windows").exists());
        assert!(!root.join("Users").exists());
        assert!(root.join("Data").join("keep.txt").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execute_rolls_back_completed_moves_when_a_later_move_fails() {
        let root = temp_root("execute-rollback");
        let desktop = root.join("Users").join("Alice").join("Desktop");
        let documents = root.join("Users").join("Alice").join("Documents");
        std::fs::create_dir_all(&desktop).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(desktop.join("desktop.txt"), b"desktop").unwrap();
        std::fs::write(documents.join("document.txt"), b"document").unwrap();
        let preserved_root = root.join("LetRecovery_Preserved_test");
        let duplicate_destination = preserved_root.join("Alice").join("Desktop");
        let plan = PreservationPlan {
            target_root: root.clone(),
            preserved_root: preserved_root.clone(),
            directories: vec![
                PreservedDirectory {
                    profile_name: "Alice".into(),
                    directory_name: "Desktop".into(),
                    source: desktop.clone(),
                    destination: duplicate_destination.clone(),
                    files: 1,
                    bytes: 7,
                },
                PreservedDirectory {
                    profile_name: "Alice".into(),
                    directory_name: "Documents".into(),
                    source: documents.clone(),
                    destination: duplicate_destination,
                    files: 1,
                    bytes: 8,
                },
            ],
            files: 2,
            bytes: 15,
            desktop_shortcuts_to_remove: Vec::new(),
            unresolved_desktop_shortcuts: 0,
        };
        let irreversible = std::cell::Cell::new(false);

        let error = execute_personal_file_preservation(&plan, || irreversible.set(true))
            .expect_err("the duplicate destination must fail before deletion");

        assert_eq!(error.stage, PreservationStage::Reversible);
        assert!(!irreversible.get());
        assert!(desktop.join("desktop.txt").is_file());
        assert!(documents.join("document.txt").is_file());
        assert!(!preserved_root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn restore_destinations(root: &Path, profile_name: &str) -> RestoreKnownFolders {
        let profile = root.join("new-users").join(profile_name);
        let public = root.join("new-users").join("Public");
        RestoreKnownFolders {
            current_profile_root: profile.clone(),
            personal: PERSONAL_DIRECTORY_NAMES.map(|name| profile.join(name)),
            public: PERSONAL_DIRECTORY_NAMES.map(|name| public.join(name)),
        }
    }

    #[test]
    fn restore_maps_an_old_profile_to_the_actual_current_profile_and_public_known_folders() {
        let root = temp_root("restore-profile-name");
        let preserved = root.join("LetRecovery_Preserved_test");
        std::fs::create_dir_all(preserved.join("VMware").join("Desktop")).unwrap();
        std::fs::create_dir_all(preserved.join("VMware").join("Documents").join("Nested")).unwrap();
        std::fs::create_dir_all(preserved.join("Public").join("Downloads")).unwrap();
        std::fs::write(
            preserved
                .join("VMware")
                .join("Desktop")
                .join("old-user.txt"),
            b"desktop",
        )
        .unwrap();
        std::fs::write(
            preserved
                .join("VMware")
                .join("Documents")
                .join("Nested")
                .join("document.txt"),
            b"document",
        )
        .unwrap();
        std::fs::write(
            preserved
                .join("Public")
                .join("Downloads")
                .join("public.bin"),
            b"public",
        )
        .unwrap();
        std::fs::write(preserved.join("README.txt"), b"readme").unwrap();
        let destinations = restore_destinations(&root, "RequestedName.WindowsChoseThisProfile");

        let report = restore_preserved_personal_files_to(&preserved, &destinations).unwrap();

        assert_eq!(report.source_profiles, 2);
        assert_eq!(
            report.current_profile_root,
            destinations.current_profile_root
        );
        assert_eq!(report.personal_directories, destinations.personal);
        assert_eq!(report.public_directories, destinations.public);
        assert!(destinations.personal[0].join("old-user.txt").is_file());
        assert!(destinations.personal[1]
            .join("Nested")
            .join("document.txt")
            .is_file());
        assert!(destinations.public[2].join("public.bin").is_file());
        assert!(!preserved.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_discards_desktop_ini_instead_of_creating_visible_conflicts() {
        let root = temp_root("restore-ignore-desktop-ini");
        let preserved = root.join("LetRecovery_Preserved_test");
        let old_personal_desktop = preserved.join("VMware").join("Desktop");
        let old_public_desktop = preserved.join("Public").join("Desktop");
        std::fs::create_dir_all(&old_personal_desktop).unwrap();
        std::fs::create_dir_all(&old_public_desktop).unwrap();
        std::fs::write(
            old_personal_desktop.join("desktop.ini"),
            b"old personal metadata",
        )
        .unwrap();
        std::fs::write(
            old_public_desktop.join("DESKTOP.INI"),
            b"old public metadata",
        )
        .unwrap();
        std::fs::write(old_personal_desktop.join("keep.txt"), b"user data").unwrap();
        std::fs::write(preserved.join("README.txt"), b"readme").unwrap();
        let destinations = restore_destinations(&root, "LRTest11");
        std::fs::create_dir_all(&destinations.personal[0]).unwrap();
        std::fs::create_dir_all(&destinations.public[0]).unwrap();
        std::fs::write(
            destinations.personal[0].join("desktop.ini"),
            b"new metadata",
        )
        .unwrap();
        std::fs::write(
            destinations.public[0].join("desktop.ini"),
            b"new public metadata",
        )
        .unwrap();

        let report = restore_preserved_personal_files_to(&preserved, &destinations).unwrap();

        assert_eq!(report.restored_files, 1);
        assert_eq!(
            std::fs::read(destinations.personal[0].join("desktop.ini")).unwrap(),
            b"new metadata"
        );
        assert_eq!(
            std::fs::read(destinations.public[0].join("desktop.ini")).unwrap(),
            b"new public metadata"
        );
        assert_eq!(
            std::fs::read(destinations.personal[0].join("keep.txt")).unwrap(),
            b"user data"
        );
        assert!(!destinations.personal[0]
            .join("desktop (from VMware).ini")
            .exists());
        assert!(!destinations.public[0]
            .join("DESKTOP (from Public).INI")
            .exists());
        assert!(!preserved.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn restore_creates_a_new_object_with_known_folder_dacl_inheritance() {
        use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
        use windows::Win32::Security::Authorization::{
            GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
        };
        use windows::Win32::Security::{
            GetSecurityDescriptorControl, ACL, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        };

        struct Descriptor(PSECURITY_DESCRIPTOR);
        impl Drop for Descriptor {
            fn drop(&mut self) {
                if !self.0 .0.is_null() {
                    unsafe {
                        let _ = LocalFree(HLOCAL(self.0 .0));
                    }
                }
            }
        }
        let dacl_is_protected = |path: &Path| {
            let mut descriptor = Descriptor(PSECURITY_DESCRIPTOR::default());
            let status = unsafe {
                GetNamedSecurityInfoW(
                    windows::core::PCWSTR(wide(path).as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    None,
                    None,
                    &mut descriptor.0,
                )
            };
            assert_eq!(status, ERROR_SUCCESS);
            let mut control = 0_u16;
            let mut revision = 0_u32;
            unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) }
                .unwrap();
            control & SE_DACL_PROTECTED.0 != 0
        };

        let root = temp_root("restore-reset-dacl");
        let preserved = root.join("LetRecovery_Preserved_test");
        let old_desktop = preserved.join("OldAlice").join("Desktop");
        std::fs::create_dir_all(&old_desktop).unwrap();
        let source = old_desktop.join("marker.txt");
        std::fs::write(&source, b"old-profile-data").unwrap();
        std::fs::write(preserved.join("README.txt"), b"readme").unwrap();

        let mut source_dacl: *mut ACL = std::ptr::null_mut();
        let mut source_descriptor = Descriptor(PSECURITY_DESCRIPTOR::default());
        let status = unsafe {
            GetNamedSecurityInfoW(
                windows::core::PCWSTR(wide(&source).as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut source_dacl),
                None,
                &mut source_descriptor.0,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        assert!(!source_dacl.is_null());
        let status = unsafe {
            SetNamedSecurityInfoW(
                windows::core::PWSTR(wide(&source).as_mut_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                PSID::default(),
                PSID::default(),
                Some(source_dacl.cast_const()),
                None,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        assert!(dacl_is_protected(&source));

        let destinations = restore_destinations(&root, "LRTest11");
        restore_preserved_personal_files_to(&preserved, &destinations).unwrap();

        let restored = destinations.personal[0].join("marker.txt");
        assert_eq!(std::fs::read(&restored).unwrap(), b"old-profile-data");
        assert!(
            !dacl_is_protected(&restored),
            "the restored file must inherit the new Known Folder DACL"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn restore_creates_a_new_file_object_instead_of_renaming_the_old_profile_object() {
        let root = temp_root("restore-new-file-object");
        let preserved = root.join("LetRecovery_Preserved_test");
        let old_desktop = preserved.join("OldAlice").join("Desktop");
        std::fs::create_dir_all(&old_desktop).unwrap();
        let old_marker = old_desktop.join("marker.txt");
        std::fs::write(&old_marker, b"old-profile-data").unwrap();
        let old_object_witness = root.join("old-object-witness.txt");
        std::fs::hard_link(&old_marker, &old_object_witness).unwrap();
        std::fs::write(preserved.join("README.txt"), b"readme").unwrap();
        let destinations = restore_destinations(&root, "LRTest11");

        restore_preserved_personal_files_to(&preserved, &destinations).unwrap();

        let restored = destinations.personal[0].join("marker.txt");
        std::fs::write(&restored, b"new-profile-write").unwrap();
        assert_eq!(
            std::fs::read(&old_object_witness).unwrap(),
            b"old-profile-data",
            "a same-volume rename would leave the restored path hard-linked to the old object"
        );
        std::fs::remove_file(old_object_witness).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn restore_removes_empty_known_folders_that_retained_windows_attributes() {
        use windows::Win32::Storage::FileSystem::{
            SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY,
            FILE_ATTRIBUTE_SYSTEM,
        };

        let root = temp_root("restore-folder-attributes");
        let preserved = root.join("LetRecovery_Preserved_test");
        let old_desktop = preserved.join("VMware").join("Desktop");
        std::fs::create_dir_all(&old_desktop).unwrap();
        std::fs::write(old_desktop.join("marker.txt"), b"desktop").unwrap();
        std::fs::write(preserved.join("README.txt"), b"readme").unwrap();
        let attributes = FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM;
        unsafe {
            SetFileAttributesW(
                windows::core::PCWSTR(wide(&old_desktop).as_ptr()),
                attributes,
            )
        }
        .unwrap();
        let destinations = restore_destinations(&root, "LRTest11");

        let report = restore_preserved_personal_files_to(&preserved, &destinations).unwrap();

        assert_eq!(report.restored_files, 1);
        assert_eq!(
            std::fs::read(destinations.personal[0].join("marker.txt")).unwrap(),
            b"desktop"
        );
        assert!(!preserved.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_never_overwrites_a_new_profile_collision() {
        let root = temp_root("restore-conflict");
        let preserved = root.join("LetRecovery_Preserved_test");
        std::fs::create_dir_all(preserved.join("OldAlice").join("Documents")).unwrap();
        std::fs::write(
            preserved
                .join("OldAlice")
                .join("Documents")
                .join("report.txt"),
            b"old",
        )
        .unwrap();
        let destinations = restore_destinations(&root, "Administrator");
        std::fs::create_dir_all(&destinations.personal[1]).unwrap();
        std::fs::write(destinations.personal[1].join("report.txt"), b"new").unwrap();

        let report = restore_preserved_personal_files_to(&preserved, &destinations).unwrap();

        assert_eq!(
            std::fs::read(destinations.personal[1].join("report.txt")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(destinations.personal[1].join("report (from OldAlice).txt")).unwrap(),
            b"old"
        );
        assert_eq!(report.renamed_conflicts, 1);
        assert!(!preserved.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
