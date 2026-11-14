//! Release manifest, pending changelog and per-file PE WIM delta planning for the incremental
//! self-updater.
//!
//! This module is pure logic shared by the release tool (`examples/release_update_tool.rs`) and
//! the normal-side updater (`正常系统端/src/core/app_update.rs`). It never touches the network or
//! the file system, so every rule — path safety, detection of a customised bundled PE,
//! removal of obsolete files and the per-file WIM diff — is covered by deterministic tests.
//!
//! Compatibility contract: `schema_version` stays 1 for additive changes. Readers ignore unknown
//! fields of the published manifest and of the image index so a newer release pipeline keeps
//! updating older clients. An incompatible format must use a new manifest URL instead of
//! changing the meaning of version 1 in place: a client that rejects the manifest can never
//! update itself again.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const PRODUCT: &str = "LetRecovery";
/// The desktop executable. The transaction always replaces it last, so "the executable was
/// replaced" implies "every other file of the release was replaced".
pub const MAIN_EXECUTABLE: &str = "LetRecovery.exe";
/// Manifest of the installed release, shipped in the package and rewritten by every update.
pub const RELEASE_MANIFEST_PATH: &str = "bin/release-manifest.json";
pub const CHANGELOG_LOCALES: [&str; 7] = [
    "zh-CN", "zh-TW", "en-US", "ja-JP", "ko-KR", "fr-FR", "de-DE",
];
pub const REQUIRED_CHANGELOG_LOCALES: [&str; 2] = ["zh-CN", "en-US"];
pub const MAX_MANIFEST_BYTES: usize = 8 << 20;
pub const MAX_IMAGE_INDEX_BYTES: u64 = 64 << 20;
/// File attributes (HIDDEN, SYSTEM, ARCHIVE, NOT_CONTENT_INDEXED) the updater restores on a
/// staged file before wimlib captures it. READONLY is deliberately excluded: it would block the
/// cleanup of the staging directory on Windows 7 and has no effect inside a booted WinPE.
pub const RESTORABLE_IMAGE_ATTRIBUTES: u32 = 0x2 | 0x4 | 0x20 | 0x2000;

const MAX_CHANGELOG_ENTRIES: usize = 200;
const MAX_CHANGELOG_TEXT_CHARS: usize = 1000;
const MAX_PACKAGE_FILES: usize = 20_000;
const MAX_IMAGE_ENTRIES: usize = 500_000;
const MAX_PATH_CHARS: usize = 400;
const MAX_COMPONENT_CHARS: usize = 255;
const MAX_FILE_SIZE: u64 = 64 << 30;
/// First-level directories that hold user or runtime state next to the executable.
const RESERVED_TOP_LEVEL_DIRECTORIES: [&str; 6] =
    ["log", "update", "downloads", "cli", "cache", "temp"];
/// Root files that belong to the user (preferences and the PE cache metadata).
const RESERVED_ROOT_FILES: [&str; 1] = ["config.json"];
const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Windows compares names case-insensitively, so every set and map in this module uses this key.
pub fn path_key(path: &str) -> String {
    path.to_lowercase()
}

/// Validates a `/`-separated relative path that will be joined below a trusted root.
///
/// Rejects absolute and drive paths, `.`/`..`, empty components, characters that Windows
/// interprets (`\ : * ? " < > |` and control characters), names Windows would silently rename
/// (trailing dot or space) and DOS device names.
pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("empty path".to_owned());
    }
    if path.chars().count() > MAX_PATH_CHARS {
        return Err(format!(
            "path is longer than {MAX_PATH_CHARS} characters: {path}"
        ));
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!(
                "path contains an empty or relative component: {path}"
            ));
        }
        if component.chars().count() > MAX_COMPONENT_CHARS {
            return Err(format!("path component is too long: {path}"));
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(format!("path component ends with a dot or space: {path}"));
        }
        if let Some(character) = component.chars().find(|character| {
            character.is_control()
                || matches!(character, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        }) {
            return Err(format!(
                "path contains the forbidden character {character:?}: {path}"
            ));
        }
        let base_name = component
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ');
        if RESERVED_DEVICE_NAMES
            .iter()
            .any(|device| base_name.eq_ignore_ascii_case(device))
        {
            return Err(format!("path names a Windows device: {path}"));
        }
    }
    Ok(())
}

/// Paths of the package that the updater must never replace or delete.
pub fn is_reserved_package_path(path: &str) -> bool {
    let reserved = match path.split_once('/') {
        Some((first, _)) => RESERVED_TOP_LEVEL_DIRECTORIES
            .iter()
            .any(|directory| first.eq_ignore_ascii_case(directory)),
        None => RESERVED_ROOT_FILES
            .iter()
            .any(|file| path.eq_ignore_ascii_case(file)),
    };
    reserved || path.eq_ignore_ascii_case(RELEASE_MANIFEST_PATH)
}

/// Payload paths listed in a manifest: safe relative paths outside the user and runtime state.
pub fn validate_package_path(path: &str) -> Result<(), String> {
    validate_relative_path(path)?;
    if is_reserved_package_path(path) {
        return Err(format!(
            "path belongs to user or runtime state and is never part of an update: {path}"
        ));
    }
    Ok(())
}

/// The bundled PE (`bin/pe/*.wim`) is a documented user-managed boundary: users may replace or
/// customise it, so an update only replaces it without asking while it is still the official
/// installed image. A customised image is never kept while the program around it is updated:
/// different program and PE versions may not work together, so the user decides first.
pub fn is_user_managed_image(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower
        .strip_prefix("bin/pe/")
        .is_some_and(|name| !name.contains('/') && name.ends_with(".wim") && name.len() > 4)
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Release versions are display labels such as `v2026.11.13`; ordering uses `build_epoch`.
pub fn validate_version(version: &str) -> Result<(), String> {
    let bytes = version.as_bytes();
    let valid = (2..=64).contains(&bytes.len())
        && bytes[0] == b'v'
        && bytes[1].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(format!("invalid release version: {version:?}"))
    }
}

// ============================================================================
// Pending changelog (repository file) and changelog entries
// ============================================================================

/// One user-facing changelog line in several languages (`locale -> text`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangelogEntry(pub BTreeMap<String, String>);

impl ChangelogEntry {
    pub fn text(&self, locale: &str) -> Option<&str> {
        self.0.get(locale).map(String::as_str)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (locale, text) in &self.0 {
            if !CHANGELOG_LOCALES.contains(&locale.as_str()) {
                return Err(format!(
                    "unsupported changelog locale {locale:?}; allowed: {}",
                    CHANGELOG_LOCALES.join(", ")
                ));
            }
            if text.trim().is_empty() {
                return Err(format!("changelog text for {locale} is empty"));
            }
            if text.trim() != text {
                return Err(format!(
                    "changelog text for {locale} has leading or trailing whitespace"
                ));
            }
            if text.chars().count() > MAX_CHANGELOG_TEXT_CHARS {
                return Err(format!(
                    "changelog text for {locale} is longer than {MAX_CHANGELOG_TEXT_CHARS} characters"
                ));
            }
            if text.chars().any(char::is_control) {
                return Err(format!(
                    "changelog text for {locale} contains a line break or control character; \
                     write one entry per change instead"
                ));
            }
        }
        for required in REQUIRED_CHANGELOG_LOCALES {
            if !self.0.contains_key(required) {
                return Err(format!(
                    "changelog entry is missing the required locale {required}"
                ));
            }
        }
        Ok(())
    }
}

/// Text to show for a UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangelogText<'a> {
    /// Show this text as is.
    Native(&'a str),
    /// Simplified Chinese source that the caller converts with the shared Windows NLS
    /// Traditional Chinese mapping, matching how the UI derives `zh-TW` from `zh-CN`.
    NeedsTraditionalConversion(&'a str),
}

pub fn changelog_text_for_language<'a>(
    entry: &'a ChangelogEntry,
    language: &str,
) -> Option<ChangelogText<'a>> {
    if let Some(text) = entry.text(language) {
        return Some(ChangelogText::Native(text));
    }
    let lower = language.to_ascii_lowercase();
    if lower.starts_with("zh") {
        let simplified = entry.text("zh-CN")?;
        let traditional =
            matches!(lower.as_str(), "zh-tw" | "zh-hk" | "zh-mo") || lower.starts_with("zh-hant");
        return Some(if traditional {
            ChangelogText::NeedsTraditionalConversion(simplified)
        } else {
            ChangelogText::Native(simplified)
        });
    }
    if lower.starts_with("ko") {
        if let Some(text) = entry.text("ko-KR") {
            return Some(ChangelogText::Native(text));
        }
    }
    entry
        .text("en-US")
        .or_else(|| entry.text("zh-CN"))
        .map(ChangelogText::Native)
}

/// `PENDING_CHANGELOG.json`: entries waiting for the next production release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingChangelog {
    pub schema_version: u32,
    pub entries: Vec<ChangelogEntry>,
}

impl PendingChangelog {
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let changelog: Self = serde_json::from_str(text)
            .map_err(|error| format!("invalid pending changelog JSON: {error}"))?;
        changelog.validate()?;
        Ok(changelog)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported pending changelog schema_version {}",
                self.schema_version
            ));
        }
        if self.entries.len() > MAX_CHANGELOG_ENTRIES {
            return Err(format!(
                "pending changelog has more than {MAX_CHANGELOG_ENTRIES} entries"
            ));
        }
        for (index, entry) in self.entries.iter().enumerate() {
            entry
                .validate()
                .map_err(|error| format!("changelog entry #{}: {error}", index + 1))?;
        }
        Ok(())
    }

    /// Canonical repository formatting: two-space indentation and a final newline.
    pub fn to_pretty_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self)
            .expect("serializing string maps and vectors cannot fail");
        text.push('\n');
        text
    }

    /// Removes exactly the entries that were published (as a multiset), keeping entries that
    /// were added on the branch while the release was being built.
    pub fn without_released(&self, released: &PendingChangelog) -> PendingChangelog {
        let mut remaining_released: HashMap<&ChangelogEntry, usize> = HashMap::new();
        for entry in &released.entries {
            *remaining_released.entry(entry).or_default() += 1;
        }
        let entries = self
            .entries
            .iter()
            .filter(|entry| match remaining_released.get_mut(entry) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    false
                }
                _ => true,
            })
            .cloned()
            .collect();
        PendingChangelog {
            schema_version: SCHEMA_VERSION,
            entries,
        }
    }
}

/// Markdown for the GitHub Release body.
pub fn render_release_notes(entries: &[ChangelogEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut notes = String::from("## 更新内容 / What's new\n\n");
    for entry in entries {
        notes.push_str("- ");
        notes.push_str(entry.text("zh-CN").unwrap_or_default());
        notes.push('\n');
        if let Some(english) = entry.text("en-US") {
            notes.push_str("  ");
            notes.push_str(english);
            notes.push('\n');
        }
    }
    notes
}

// ============================================================================
// Published update manifest
// ============================================================================

/// A content-addressed object on the update CDN (`<object base URL><sha256>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub sha256: String,
    pub size: u64,
}

impl ObjectRef {
    fn validate(&self, what: &str, max_size: u64) -> Result<(), String> {
        if !is_lower_hex(&self.sha256, 64) {
            return Err(format!("{what} has an invalid SHA-256"));
        }
        if self.size == 0 || self.size > max_size {
            return Err(format!("{what} has an invalid size {}", self.size));
        }
        Ok(())
    }
}

/// One file of the release package. The object holding its bytes is `sha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// Per-file index of the bundled PE WIM; lets an official older image be patched instead of
    /// downloading the whole WIM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wim_index: Option<ObjectRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub product: String,
    pub version: String,
    /// `SOURCE_DATE_EPOCH` of the release build; equals the executable's `BUILD_EPOCH`.
    pub build_epoch: u64,
    #[serde(default)]
    pub changelog: Vec<ChangelogEntry>,
    pub files: Vec<PackageFile>,
}

impl UpdateManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "update manifest is larger than {MAX_MANIFEST_BYTES} bytes"
            ));
        }
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid update manifest JSON: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported update manifest schema_version {}",
                self.schema_version
            ));
        }
        if self.product != PRODUCT {
            return Err(format!(
                "update manifest is for {:?}, not {PRODUCT}",
                self.product
            ));
        }
        validate_version(&self.version)?;
        if self.build_epoch == 0 {
            return Err("update manifest has no build_epoch".to_owned());
        }
        if self.changelog.len() > MAX_CHANGELOG_ENTRIES {
            return Err("update manifest changelog is too long".to_owned());
        }
        for (index, entry) in self.changelog.iter().enumerate() {
            entry
                .validate()
                .map_err(|error| format!("manifest changelog entry #{}: {error}", index + 1))?;
        }
        if self.files.is_empty() || self.files.len() > MAX_PACKAGE_FILES {
            return Err(format!("update manifest lists {} files", self.files.len()));
        }
        let mut seen = HashSet::new();
        for file in &self.files {
            validate_package_path(&file.path)?;
            if !seen.insert(path_key(&file.path)) {
                return Err(format!("update manifest lists {} twice", file.path));
            }
            if file.size > MAX_FILE_SIZE {
                return Err(format!("{} is implausibly large", file.path));
            }
            if !is_lower_hex(&file.sha256, 64) {
                return Err(format!("{} has an invalid SHA-256", file.path));
            }
            if let Some(index) = &file.wim_index {
                if !is_user_managed_image(&file.path) {
                    return Err(format!(
                        "{} carries a WIM index, which is only published for the bundled PE",
                        file.path
                    ));
                }
                index.validate(
                    &format!("WIM index of {}", file.path),
                    MAX_IMAGE_INDEX_BYTES,
                )?;
            }
        }
        if !seen.contains(&path_key(MAIN_EXECUTABLE)) {
            return Err(format!("update manifest does not list {MAIN_EXECUTABLE}"));
        }
        Ok(())
    }

    pub fn file(&self, path: &str) -> Option<&PackageFile> {
        let key = path_key(path);
        self.files.iter().find(|file| path_key(&file.path) == key)
    }

    pub fn is_newer_than(&self, local_build_epoch: u64) -> bool {
        self.build_epoch > local_build_epoch
    }

    /// Records the identity of a locally rebuilt PE image so the next update still recognises it
    /// as the official installed image rather than a user customisation.
    pub fn set_local_identity(&mut self, path: &str, size: u64, sha256: &str) -> bool {
        let key = path_key(path);
        match self
            .files
            .iter_mut()
            .find(|file| path_key(&file.path) == key)
        {
            Some(file) => {
                file.size = size;
                file.sha256 = sha256.to_owned();
                true
            }
            None => false,
        }
    }

    pub fn to_pretty_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self)
            .expect("serializing string maps and vectors cannot fail");
        text.push('\n');
        text
    }
}

// ============================================================================
// Package planning
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFileState {
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageAction {
    Unchanged,
    /// Download the complete object.
    Download,
    /// Rebuild the official installed PE image from its per-file index.
    PatchImage,
    /// The bundled PE was customised by the user. The new program may not work with it, so the
    /// updater neither keeps it silently nor overwrites it: the user either lets the complete
    /// official image replace it or keeps it and does not update.
    CustomizedImage,
    /// File of the installed release that the new release no longer ships.
    DeleteObsolete,
}

/// Decides how one file of the new release reaches the disk.
///
/// `installed` is the entry of the installed release manifest for the same path. The bundled PE
/// image is only replaced without asking while it still equals that official entry; any other
/// content is a user customisation (`CustomizedImage`) that the caller must neither keep nor
/// replace without the user's decision.
pub fn plan_package_file(
    target: &PackageFile,
    local: Option<&LocalFileState>,
    installed: Option<&PackageFile>,
) -> PackageAction {
    let Some(local) = local else {
        return PackageAction::Download;
    };
    if local.size == target.size && local.sha256 == target.sha256 {
        return PackageAction::Unchanged;
    }
    if is_user_managed_image(&target.path) {
        let official =
            installed.is_some_and(|entry| entry.size == local.size && entry.sha256 == local.sha256);
        return if !official {
            PackageAction::CustomizedImage
        } else if target.wim_index.is_some() {
            PackageAction::PatchImage
        } else {
            PackageAction::Download
        };
    }
    PackageAction::Download
}

/// A file of the installed release missing from the new release is removed only while it still
/// has the official installed content; anything the user changed is left alone.
pub fn plan_obsolete_file(
    installed: &PackageFile,
    local: Option<&LocalFileState>,
) -> PackageAction {
    if is_reserved_package_path(&installed.path)
        || installed.path.eq_ignore_ascii_case(MAIN_EXECUTABLE)
    {
        return PackageAction::Unchanged;
    }
    match local {
        Some(local) if local.size == installed.size && local.sha256 == installed.sha256 => {
            PackageAction::DeleteObsolete
        }
        _ => PackageAction::Unchanged,
    }
}

// ============================================================================
// PE WIM per-file index and patch planning
// ============================================================================

/// `[path, size, sha1, sha256, attributes]` of one regular file in the image. SHA-1 is the WIM's
/// own stream hash (cheap to read from the local image metadata); SHA-256 names the CDN object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageFileEntry(pub String, pub u64, pub String, pub String, pub u32);

impl ImageFileEntry {
    pub fn path(&self) -> &str {
        &self.0
    }
    pub fn size(&self) -> u64 {
        self.1
    }
    pub fn sha1(&self) -> &str {
        &self.2
    }
    pub fn sha256(&self) -> &str {
        &self.3
    }
    pub fn attributes(&self) -> u32 {
        self.4
    }
}

/// Complete content of one image of the official PE WIM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WimContentIndex {
    pub schema_version: u32,
    pub image: u32,
    pub boot_index: u32,
    pub directories: Vec<String>,
    pub files: Vec<ImageFileEntry>,
}

impl WimContentIndex {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_IMAGE_INDEX_BYTES {
            return Err("WIM index is too large".to_owned());
        }
        let index: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid WIM index JSON: {error}"))?;
        index.validate()?;
        Ok(index)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported WIM index schema_version {}",
                self.schema_version
            ));
        }
        if self.image == 0 {
            return Err("WIM index image number must start at 1".to_owned());
        }
        if self.directories.len() + self.files.len() > MAX_IMAGE_ENTRIES {
            return Err("WIM index has too many entries".to_owned());
        }
        let mut directories = HashSet::new();
        for directory in &self.directories {
            validate_relative_path(directory)?;
            if !directories.insert(path_key(directory)) {
                return Err(format!("WIM index lists directory {directory} twice"));
            }
        }
        for directory in &self.directories {
            if let Some((parent, _)) = directory.rsplit_once('/') {
                if !directories.contains(&path_key(parent)) {
                    return Err(format!("WIM index misses the parent of {directory}"));
                }
            }
        }
        let mut files = HashSet::new();
        for file in &self.files {
            validate_relative_path(file.path())?;
            let key = path_key(file.path());
            if directories.contains(&key) || !files.insert(key) {
                return Err(format!("WIM index lists {} twice", file.path()));
            }
            if let Some((parent, _)) = file.path().rsplit_once('/') {
                if !directories.contains(&path_key(parent)) {
                    return Err(format!("WIM index misses the parent of {}", file.path()));
                }
            }
            if file.size() > MAX_FILE_SIZE
                || !is_lower_hex(file.sha1(), 40)
                || !is_lower_hex(file.sha256(), 64)
            {
                return Err(format!("WIM index entry {} is invalid", file.path()));
            }
        }
        Ok(())
    }

    pub fn to_compact_json(&self) -> String {
        serde_json::to_string(self).expect("serializing strings and integers cannot fail")
    }
}

/// One entry of a local image as reported by wimlib.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageEntryKind {
    Directory,
    File {
        size: u64,
        sha1: String,
    },
    /// Reparse points, named data streams or undecodable names: never kept by a patch.
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEntry {
    pub path: String,
    pub attributes: u32,
    pub kind: ImageEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDelete {
    pub path: String,
    pub recursive: bool,
}

/// wimlib update commands that turn a local image into the indexed official image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImagePatchPlan {
    pub delete: Vec<ImageDelete>,
    /// Missing directories, parents before children.
    pub add_directories: Vec<String>,
    /// New or changed regular files (an ADD replaces an existing non-directory).
    pub add_files: Vec<ImageFileEntry>,
}

impl ImagePatchPlan {
    pub fn is_empty(&self) -> bool {
        self.delete.is_empty() && self.add_directories.is_empty() && self.add_files.is_empty()
    }

    /// Objects to download, once per distinct content.
    pub fn required_objects(&self) -> Vec<ObjectRef> {
        let mut seen = HashSet::new();
        self.add_files
            .iter()
            .filter(|file| seen.insert(file.sha256().to_owned()))
            .map(|file| ObjectRef {
                sha256: file.sha256().to_owned(),
                size: file.size(),
            })
            .collect()
    }
}

fn has_removed_ancestor(key: &str, removed: &HashSet<String>) -> bool {
    let mut current = key;
    while let Some((parent, _)) = current.rsplit_once('/') {
        if removed.contains(parent) {
            return true;
        }
        current = parent;
    }
    false
}

pub fn plan_image_patch(local: &[ImageEntry], target: &WimContentIndex) -> ImagePatchPlan {
    let target_directories: HashSet<String> = target
        .directories
        .iter()
        .map(|path| path_key(path))
        .collect();
    let target_files: HashMap<String, &ImageFileEntry> = target
        .files
        .iter()
        .map(|file| (path_key(file.path()), file))
        .collect();
    let local_by_key: HashMap<String, &ImageEntry> = local
        .iter()
        .map(|entry| (path_key(&entry.path), entry))
        .collect();

    let mut candidates: Vec<(String, &ImageEntry, bool)> = local
        .iter()
        .filter_map(|entry| {
            let key = path_key(&entry.path);
            let remove = match &entry.kind {
                ImageEntryKind::Directory => !target_directories.contains(&key),
                ImageEntryKind::File { .. } => !target_files.contains_key(&key),
                ImageEntryKind::Unsupported(_) => true,
            };
            let recursive = !matches!(entry.kind, ImageEntryKind::File { .. });
            remove.then_some((key, entry, recursive))
        })
        .collect();
    // Sorting by key places every directory before its descendants.
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    let mut removed = HashSet::new();
    let mut delete = Vec::new();
    for (key, entry, recursive) in candidates {
        if has_removed_ancestor(&key, &removed) {
            continue;
        }
        if recursive {
            removed.insert(key);
        }
        delete.push(ImageDelete {
            path: entry.path.clone(),
            recursive,
        });
    }
    let is_removed = |key: &str| removed.contains(key) || has_removed_ancestor(key, &removed);

    let mut add_directories: Vec<String> = target
        .directories
        .iter()
        .filter(|directory| {
            let key = path_key(directory);
            !matches!(
                local_by_key.get(&key).map(|entry| &entry.kind),
                Some(ImageEntryKind::Directory)
            ) || is_removed(&key)
        })
        .cloned()
        .collect();
    add_directories.sort_by(|left, right| {
        left.matches('/')
            .count()
            .cmp(&right.matches('/').count())
            .then_with(|| path_key(left).cmp(&path_key(right)))
    });

    let mut add_files: Vec<ImageFileEntry> = target
        .files
        .iter()
        .filter(|file| {
            let key = path_key(file.path());
            let unchanged = match local_by_key.get(&key).map(|entry| &entry.kind) {
                Some(ImageEntryKind::File { size, sha1 }) => {
                    *size == file.size() && sha1 == file.sha1()
                }
                _ => false,
            };
            !unchanged || is_removed(&key)
        })
        .cloned()
        .collect();
    add_files.sort_by_key(|file| path_key(file.path()));

    ImagePatchPlan {
        delete,
        add_directories,
        add_files,
    }
}

/// Proves that a (patched) image contains exactly the indexed directories and files.
pub fn verify_image_listing(local: &[ImageEntry], target: &WimContentIndex) -> Result<(), String> {
    let directories: HashSet<String> = target
        .directories
        .iter()
        .map(|path| path_key(path))
        .collect();
    let files: HashMap<String, &ImageFileEntry> = target
        .files
        .iter()
        .map(|file| (path_key(file.path()), file))
        .collect();
    let mut seen = HashSet::new();
    for entry in local {
        let key = path_key(&entry.path);
        if !seen.insert(key.clone()) {
            return Err(format!("image contains {} twice", entry.path));
        }
        match &entry.kind {
            ImageEntryKind::Directory => {
                if !directories.contains(&key) {
                    return Err(format!("unexpected directory {}", entry.path));
                }
            }
            ImageEntryKind::File { size, sha1 } => match files.get(&key) {
                Some(expected) if expected.size() == *size && expected.sha1() == sha1 => {}
                Some(_) => return Err(format!("content of {} differs", entry.path)),
                None => return Err(format!("unexpected file {}", entry.path)),
            },
            ImageEntryKind::Unsupported(reason) => {
                return Err(format!("unsupported entry {}: {reason}", entry.path));
            }
        }
    }
    if seen.len() != directories.len() + files.len() {
        return Err(format!(
            "image has {} entries, the official index has {}",
            seen.len(),
            directories.len() + files.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(fill: char, length: usize) -> String {
        std::iter::repeat_n(fill, length).collect()
    }

    fn entry(pairs: &[(&str, &str)]) -> ChangelogEntry {
        ChangelogEntry(
            pairs
                .iter()
                .map(|(locale, text)| (locale.to_string(), text.to_string()))
                .collect(),
        )
    }

    fn package(path: &str, fill: char) -> PackageFile {
        PackageFile {
            path: path.to_owned(),
            size: 10,
            sha256: sha(fill, 64),
            wim_index: None,
        }
    }

    fn local(fill: char) -> LocalFileState {
        LocalFileState {
            size: 10,
            sha256: sha(fill, 64),
        }
    }

    fn manifest(files: Vec<PackageFile>) -> UpdateManifest {
        UpdateManifest {
            schema_version: 1,
            product: PRODUCT.to_owned(),
            version: "v2026.11.13".to_owned(),
            build_epoch: 1_794_567_890,
            changelog: vec![entry(&[("zh-CN", "修复"), ("en-US", "Fix")])],
            files,
        }
    }

    #[test]
    fn relative_paths_reject_traversal_devices_and_windows_renames() {
        for valid in [
            "LetRecovery.exe",
            "bin/pe/LetRecovery_PE.wim",
            "Program Files/LetRecoveryPE/LetRecoveryPE.exe",
            "Users/Default/AppData/Roaming/Microsoft/Windows/SendTo/桌面快捷方式.DeskLink",
            "Windows/System32/DriverStore/FileRepository/a.inf_amd64_973fa8643d2ac3e6/a.sys",
        ] {
            assert_eq!(validate_relative_path(valid), Ok(()), "{valid}");
        }
        for invalid in [
            "",
            "/bin/x",
            "bin/",
            "bin//x",
            "../x",
            "bin/./x",
            "C:/x",
            "bin\\x",
            "bin/x.",
            "bin/x ",
            "bin/NUL",
            "bin/com1.txt",
            "bin/a*b",
            "bin/a\nb",
        ] {
            assert!(validate_relative_path(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn package_paths_never_cover_user_state() {
        for reserved in [
            "config.json",
            "CONFIG.JSON",
            "log/LetRecovery.log",
            "Update/journal.json",
            "downloads/x.iso",
            "cli/install.json",
            "cache/pe/x.wim",
            "temp/x",
            "bin/release-manifest.json",
        ] {
            assert!(validate_package_path(reserved).is_err(), "{reserved}");
        }
        assert!(validate_package_path("bin/config.json").is_ok());
        assert!(validate_package_path("bin/lang/en-US.json").is_ok());
    }

    #[test]
    fn only_the_bundled_pe_directory_is_user_managed() {
        assert!(is_user_managed_image("bin/pe/LetRecovery_PE.wim"));
        assert!(is_user_managed_image("BIN/PE/Custom.WIM"));
        assert!(!is_user_managed_image(
            "bin/pca2023/pca2023-modern-amd64.wim"
        ));
        assert!(!is_user_managed_image("bin/pe/sub/x.wim"));
        assert!(!is_user_managed_image("bin/pe/.wim"));
    }

    #[test]
    fn versions_are_tag_labels() {
        assert!(validate_version("v2026.11.13").is_ok());
        assert!(validate_version("v2026.11.13-hotfix.1").is_ok());
        for invalid in ["2026.11.13", "v", "v.1", "v2026 11", "v2026/11"] {
            assert!(validate_version(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn pending_changelog_requires_chinese_and_english_single_lines() {
        let valid = PendingChangelog::parse(
            "\u{feff}{\"schema_version\":1,\"entries\":[{\"zh-CN\":\"新增\",\"en-US\":\"Added\",\"ja-JP\":\"追加\"}]}",
        )
        .unwrap();
        assert_eq!(valid.entries.len(), 1);
        for invalid in [
            r#"{"schema_version":1,"entries":[{"zh-CN":"只有中文"}]}"#,
            r#"{"schema_version":1,"entries":[{"zh-CN":"a","en-US":"b","xx-XX":"c"}]}"#,
            r#"{"schema_version":1,"entries":[{"zh-CN":" a","en-US":"b"}]}"#,
            r#"{"schema_version":1,"entries":[{"zh-CN":"a\nb","en-US":"b"}]}"#,
            r#"{"schema_version":1,"entries":[{"zh-CN":"","en-US":"b"}]}"#,
            r#"{"schema_version":2,"entries":[]}"#,
            r#"{"schema_version":1,"entries":[],"extra":true}"#,
        ] {
            assert!(PendingChangelog::parse(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            PendingChangelog::empty().to_pretty_json(),
            "{\n  \"schema_version\": 1,\n  \"entries\": []\n}\n"
        );
    }

    #[test]
    fn clearing_removes_only_released_entries_as_a_multiset() {
        let a = entry(&[("zh-CN", "甲"), ("en-US", "A")]);
        let b = entry(&[("zh-CN", "乙"), ("en-US", "B")]);
        let c = entry(&[("zh-CN", "丙"), ("en-US", "C")]);
        let released = PendingChangelog {
            schema_version: 1,
            entries: vec![a.clone(), b.clone()],
        };
        // `c` and the second `a` were added while the release was being built.
        let current = PendingChangelog {
            schema_version: 1,
            entries: vec![a.clone(), b.clone(), c.clone(), a.clone()],
        };
        assert_eq!(
            current.without_released(&released).entries,
            vec![c, a.clone()]
        );
        assert!(released.without_released(&released).entries.is_empty());
    }

    #[test]
    fn release_notes_list_chinese_and_english() {
        let notes = render_release_notes(&[entry(&[("zh-CN", "修复"), ("en-US", "Fix")])]);
        assert!(notes.contains("- 修复\n  Fix\n"));
        assert!(render_release_notes(&[]).is_empty());
    }

    #[test]
    fn changelog_language_selection_falls_back_deterministically() {
        let full = entry(&[("zh-CN", "中"), ("en-US", "en"), ("ja-JP", "ja")]);
        assert_eq!(
            changelog_text_for_language(&full, "ja-JP"),
            Some(ChangelogText::Native("ja"))
        );
        assert_eq!(
            changelog_text_for_language(&full, "zh-TW"),
            Some(ChangelogText::NeedsTraditionalConversion("中"))
        );
        assert_eq!(
            changelog_text_for_language(&full, "zh-CN"),
            Some(ChangelogText::Native("中"))
        );
        assert_eq!(
            changelog_text_for_language(&full, "de-DE"),
            Some(ChangelogText::Native("en"))
        );
        assert_eq!(
            changelog_text_for_language(&full, "ko-KP"),
            Some(ChangelogText::Native("en"))
        );
    }

    #[test]
    fn manifest_validation_rejects_unsafe_or_ambiguous_listings() {
        let exe = package(MAIN_EXECUTABLE, 'a');
        assert!(manifest(vec![exe.clone()]).validate().is_ok());
        assert!(manifest(vec![package("bin/x.dll", 'b')])
            .validate()
            .is_err());
        assert!(manifest(vec![exe.clone(), package("letrecovery.EXE", 'b')])
            .validate()
            .is_err());
        assert!(manifest(vec![exe.clone(), package("config.json", 'b')])
            .validate()
            .is_err());
        let mut indexed_locked = package("bin/pca2023/x.wim", 'c');
        indexed_locked.wim_index = Some(ObjectRef {
            sha256: sha('d', 64),
            size: 5,
        });
        assert!(manifest(vec![exe.clone(), indexed_locked])
            .validate()
            .is_err());
        let mut indexed_pe = package("bin/pe/LetRecovery_PE.wim", 'c');
        indexed_pe.wim_index = Some(ObjectRef {
            sha256: sha('d', 64),
            size: 5,
        });
        let valid = manifest(vec![exe, indexed_pe]);
        assert!(valid.validate().is_ok());
        // Unknown fields are ignored so newer pipelines keep updating older clients.
        let mut json: serde_json::Value = serde_json::from_str(&valid.to_pretty_json()).unwrap();
        json["future_field"] = serde_json::json!({"anything": 1});
        let parsed = UpdateManifest::parse(json.to_string().as_bytes()).unwrap();
        assert_eq!(parsed, valid);
        assert!(parsed.is_newer_than(1));
        assert!(!parsed.is_newer_than(parsed.build_epoch));
    }

    #[test]
    fn package_planning_flags_customised_pe_and_patches_official_pe() {
        let mut pe = package("bin/pe/LetRecovery_PE.wim", 'n');
        pe.wim_index = Some(ObjectRef {
            sha256: sha('i', 64),
            size: 7,
        });
        let installed_pe = package("bin/pe/LetRecovery_PE.wim", 'o');
        assert_eq!(
            plan_package_file(&pe, Some(&local('n')), Some(&installed_pe)),
            PackageAction::Unchanged
        );
        assert_eq!(
            plan_package_file(&pe, Some(&local('o')), Some(&installed_pe)),
            PackageAction::PatchImage
        );
        assert_eq!(
            plan_package_file(&pe, Some(&local('u')), Some(&installed_pe)),
            PackageAction::CustomizedImage
        );
        assert_eq!(
            plan_package_file(&pe, Some(&local('o')), None),
            PackageAction::CustomizedImage
        );
        assert_eq!(
            plan_package_file(&pe, None, Some(&installed_pe)),
            PackageAction::Download
        );
        pe.wim_index = None;
        assert_eq!(
            plan_package_file(&pe, Some(&local('o')), Some(&installed_pe)),
            PackageAction::Download
        );
        let exe = package(MAIN_EXECUTABLE, 'n');
        assert_eq!(
            plan_package_file(&exe, Some(&local('u')), None),
            PackageAction::Download
        );
    }

    #[test]
    fn obsolete_files_are_removed_only_when_unmodified() {
        let old = package("bin/old.dll", 'o');
        assert_eq!(
            plan_obsolete_file(&old, Some(&local('o'))),
            PackageAction::DeleteObsolete
        );
        assert_eq!(
            plan_obsolete_file(&old, Some(&local('u'))),
            PackageAction::Unchanged
        );
        assert_eq!(plan_obsolete_file(&old, None), PackageAction::Unchanged);
        assert_eq!(
            plan_obsolete_file(&package(MAIN_EXECUTABLE, 'o'), Some(&local('o'))),
            PackageAction::Unchanged
        );
    }

    fn file(path: &str, fill: char) -> ImageEntry {
        ImageEntry {
            path: path.to_owned(),
            attributes: 0x20,
            kind: ImageEntryKind::File {
                size: 3,
                sha1: sha(fill, 40),
            },
        }
    }

    fn directory(path: &str) -> ImageEntry {
        ImageEntry {
            path: path.to_owned(),
            attributes: 0x10,
            kind: ImageEntryKind::Directory,
        }
    }

    fn indexed(path: &str, fill: char) -> ImageFileEntry {
        ImageFileEntry(path.to_owned(), 3, sha(fill, 40), sha(fill, 64), 0x20)
    }

    fn index(directories: &[&str], files: Vec<ImageFileEntry>) -> WimContentIndex {
        WimContentIndex {
            schema_version: 1,
            image: 1,
            boot_index: 1,
            directories: directories.iter().map(|path| path.to_string()).collect(),
            files,
        }
    }

    #[test]
    fn image_plan_changes_only_the_differing_pe_files() {
        let local = vec![
            directory("Program Files"),
            directory("Program Files/LetRecoveryPE"),
            file("Program Files/LetRecoveryPE/LetRecoveryPE.exe", 'a'),
            directory("Windows"),
            file("Windows/PECMD.INI", 'b'),
            file("Windows/kept.dll", 'c'),
            directory("Windows/Old"),
            file("Windows/Old/x.sys", 'd'),
            file("Windows/removed.txt", 'e'),
        ];
        let target = index(
            &[
                "Program Files",
                "Program Files/LetRecoveryPE",
                "Users",
                "Users/Default",
                "Windows",
            ],
            vec![
                indexed(
                    "Program Files/LetRecoveryPE/LetRecoveryPE.exe",
                    'A'.to_ascii_lowercase(),
                ),
                indexed("Program Files/LetRecoveryPE/handoff-capabilities.txt", 'f'),
                indexed("Windows/PECMD.INI", '1'),
                indexed("Windows/kept.dll", 'c'),
            ],
        );
        target.validate().unwrap();
        let plan = plan_image_patch(&local, &target);
        assert_eq!(
            plan.delete,
            vec![
                ImageDelete {
                    path: "Windows/Old".to_owned(),
                    recursive: true
                },
                ImageDelete {
                    path: "Windows/removed.txt".to_owned(),
                    recursive: false
                },
            ]
        );
        assert_eq!(plan.add_directories, vec!["Users", "Users/Default"]);
        let added: Vec<&str> = plan.add_files.iter().map(|file| file.path()).collect();
        assert_eq!(
            added,
            vec![
                "Program Files/LetRecoveryPE/handoff-capabilities.txt",
                "Windows/PECMD.INI"
            ]
        );
        assert_eq!(plan.required_objects().len(), 2);
    }

    #[test]
    fn image_plan_handles_kind_changes_and_case_insensitive_names() {
        let local = vec![
            directory("Windows"),
            directory("Windows/Thing"),
            file("Windows/Thing/inner.txt", 'a'),
            file("WINDOWS/pecmd.ini", 'b'),
            ImageEntry {
                path: "Windows/link".to_owned(),
                attributes: 0x400,
                kind: ImageEntryKind::Unsupported("reparse point".to_owned()),
            },
        ];
        let target = index(
            &["Windows"],
            vec![
                indexed("Windows/Thing", 'c'),
                indexed("Windows/PECMD.INI", 'b'),
            ],
        );
        let plan = plan_image_patch(&local, &target);
        assert_eq!(
            plan.delete,
            vec![
                ImageDelete {
                    path: "Windows/link".to_owned(),
                    recursive: true
                },
                ImageDelete {
                    path: "Windows/Thing".to_owned(),
                    recursive: true
                },
            ]
        );
        assert!(plan.add_directories.is_empty());
        assert_eq!(plan.add_files, vec![indexed("Windows/Thing", 'c')]);
    }

    #[test]
    fn identical_image_needs_no_patch_and_verifies() {
        let target = index(&["Windows"], vec![indexed("Windows/a.dll", 'a')]);
        let local = vec![directory("Windows"), file("Windows/a.dll", 'a')];
        assert!(plan_image_patch(&local, &target).is_empty());
        assert_eq!(verify_image_listing(&local, &target), Ok(()));
        let mut changed = local.clone();
        changed[1] = file("Windows/a.dll", 'b');
        assert!(verify_image_listing(&changed, &target).is_err());
        assert!(verify_image_listing(&local[..1], &target).is_err());
        let mut extra = local.clone();
        extra.push(file("Windows/extra.dll", 'c'));
        assert!(verify_image_listing(&extra, &target).is_err());
        let mut duplicate = local;
        duplicate.push(file("WINDOWS/A.DLL", 'a'));
        assert!(verify_image_listing(&duplicate, &target).is_err());
    }

    #[test]
    fn image_index_rejects_orphans_and_duplicates() {
        assert!(index(&["Windows"], vec![indexed("Windows/a", 'a')])
            .validate()
            .is_ok());
        assert!(index(&[], vec![indexed("Windows/a", 'a')])
            .validate()
            .is_err());
        assert!(index(&["Windows", "WINDOWS"], vec![]).validate().is_err());
        assert!(index(&["Windows"], vec![indexed("windows", 'a')])
            .validate()
            .is_err());
        let round_trip = WimContentIndex::parse(
            index(&["Windows"], vec![indexed("Windows/a", 'a')])
                .to_compact_json()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(round_trip.files.len(), 1);
    }
}
