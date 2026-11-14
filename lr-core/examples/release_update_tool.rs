//! Release-pipeline helper for the incremental self-updater.
//!
//! ```text
//! release_update_tool validate-changelog <PENDING_CHANGELOG.json>
//! release_update_tool release-notes <PENDING_CHANGELOG.json> <out.md>
//! release_update_tool clear-changelog <released.json> <current.json>
//! release_update_tool generate <package-root> <PENDING_CHANGELOG.json> <version> <build-epoch> <objects-dir>
//! ```
//!
//! `generate` hashes every file of the final release tree except user state, stores each distinct
//! content once as `<objects-dir>/<sha256>`, builds the per-file index of the bundled PE WIM with
//! the same `WimlibManager::list_image_entries` the desktop updater uses for its comparison, and
//! writes `bin/release-manifest.json` into the package. The objects directory is then uploaded to
//! the content-addressed CDN prefix and the manifest is published through the authenticated
//! release callback.

use std::fs;
use std::path::{Path, PathBuf};

use lr_core::update_manifest::{
    is_user_managed_image, render_release_notes, validate_package_path, validate_relative_path,
    validate_version, ImageEntryKind, ImageFileEntry, ObjectRef, PackageFile, PendingChangelog,
    UpdateManifest, WimContentIndex, PRODUCT, RELEASE_MANIFEST_PATH, SCHEMA_VERSION,
};

const USAGE: &str = "usage:
  release_update_tool validate-changelog <PENDING_CHANGELOG.json>
  release_update_tool release-notes <PENDING_CHANGELOG.json> <out.md>
  release_update_tool clear-changelog <released.json> <current.json>
  release_update_tool generate <package-root> <PENDING_CHANGELOG.json> <version> <build-epoch> <objects-dir>";

fn main() {
    if let Err(error) = run() {
        eprintln!("release_update_tool: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["validate-changelog", path] => {
            let changelog = read_changelog(Path::new(path))?;
            println!(
                "{path}: valid, {} pending entr{}",
                changelog.entries.len(),
                if changelog.entries.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
            Ok(())
        }
        ["release-notes", path, output] => {
            let changelog = read_changelog(Path::new(path))?;
            write_text(Path::new(output), &render_release_notes(&changelog.entries))
        }
        ["clear-changelog", released, current] => {
            let released = read_changelog(Path::new(released))?;
            let current_path = Path::new(current);
            let current = read_changelog(current_path)?;
            let remaining = current.without_released(&released);
            println!(
                "removed {} released entr{}, kept {}",
                current.entries.len() - remaining.entries.len(),
                if current.entries.len() - remaining.entries.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                remaining.entries.len()
            );
            write_text(current_path, &remaining.to_pretty_json())
        }
        ["generate", root, changelog, version, build_epoch, objects] => generate(
            Path::new(root),
            Path::new(changelog),
            version,
            build_epoch,
            Path::new(objects),
        ),
        _ => Err(USAGE.to_owned()),
    }
}

fn read_changelog(path: &Path) -> Result<PendingChangelog, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    PendingChangelog::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn relative_slash_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?;
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(name) => components.push(
                name.to_str()
                    .ok_or_else(|| format!("{} is not valid Unicode", path.display()))?
                    .to_owned(),
            ),
            _ => return Err(format!("unexpected path component in {}", path.display())),
        }
    }
    Ok(components.join("/"))
}

/// Stores `source` as `<objects>/<sha256>` once and returns its size and SHA-256.
fn store_object(source: &Path, objects: &Path) -> Result<(u64, String), String> {
    let size = fs::metadata(source)
        .map_err(|error| format!("cannot stat {}: {error}", source.display()))?
        .len();
    let sha256 = lr_core::hash::sha256_file(source, |_| {})
        .map_err(|error| format!("cannot hash {}: {error}", source.display()))?;
    let destination = objects.join(&sha256);
    match fs::metadata(&destination) {
        Ok(existing) if existing.len() == size => {}
        Ok(_) => {
            return Err(format!(
                "object {} already exists with a different size",
                destination.display()
            ))
        }
        Err(_) => {
            let temporary = objects.join(format!("{sha256}.partial"));
            fs::copy(source, &temporary)
                .map_err(|error| format!("cannot copy {}: {error}", source.display()))?;
            fs::rename(&temporary, &destination)
                .map_err(|error| format!("cannot publish object {sha256}: {error}"))?;
        }
    }
    Ok((size, sha256))
}

/// Builds the per-file index of image 1 of the bundled PE and stores every file as an object.
fn build_image_index(wim: &Path, objects: &Path) -> Result<ObjectRef, String> {
    let wim_path = wim
        .to_str()
        .ok_or_else(|| format!("{} is not valid Unicode", wim.display()))?;
    let manager = lr_core::wimlib::WimlibManager::new()?;
    let listing = manager.list_image_entries(wim_path, 1)?;
    if listing.image_count != 1 {
        return Err(format!(
            "{} has {} images; only single-image PE WIMs get a per-file index",
            wim.display(),
            listing.image_count
        ));
    }
    if let Some(entry) = listing
        .entries
        .iter()
        .find(|entry| matches!(entry.kind, ImageEntryKind::Unsupported(_)))
    {
        return Err(format!(
            "{} contains an entry the patcher cannot reproduce: {} ({:?})",
            wim.display(),
            entry.path,
            entry.kind
        ));
    }
    let extract_root = std::env::temp_dir().join(format!(
        "lr-update-index-{}-{}",
        std::process::id(),
        listing.entries.len()
    ));
    if extract_root.exists() {
        fs::remove_dir_all(&extract_root)
            .map_err(|error| format!("cannot reset {}: {error}", extract_root.display()))?;
    }
    fs::create_dir_all(&extract_root)
        .map_err(|error| format!("cannot create {}: {error}", extract_root.display()))?;
    let extract_root_text = extract_root
        .to_str()
        .ok_or_else(|| "temporary directory is not valid Unicode".to_owned())?
        .to_owned();
    let result = (|| {
        // wimlib verifies every stream against its SHA-1 while extracting.
        manager.apply_image(wim_path, &extract_root_text, 1, None)?;
        let mut directories = Vec::new();
        let mut files = Vec::new();
        for entry in &listing.entries {
            validate_relative_path(&entry.path)?;
            match &entry.kind {
                ImageEntryKind::Directory => directories.push(entry.path.clone()),
                ImageEntryKind::File { size, sha1 } => {
                    let extracted: PathBuf = entry
                        .path
                        .split('/')
                        .fold(extract_root.clone(), |path, component| path.join(component));
                    let (actual_size, sha256) = store_object(&extracted, objects)?;
                    if actual_size != *size {
                        return Err(format!(
                            "extracted {} has {actual_size} bytes, the image declares {size}",
                            entry.path
                        ));
                    }
                    files.push(ImageFileEntry(
                        entry.path.clone(),
                        *size,
                        sha1.clone(),
                        sha256,
                        entry.attributes,
                    ));
                }
                ImageEntryKind::Unsupported(_) => unreachable!("rejected above"),
            }
        }
        let index = WimContentIndex {
            schema_version: SCHEMA_VERSION,
            image: 1,
            boot_index: listing.boot_index,
            directories,
            files,
        };
        index.validate()?;
        let bytes = index.to_compact_json().into_bytes();
        let sha256 = lr_core::hash::sha256_bytes(&bytes);
        let destination = objects.join(&sha256);
        fs::write(&destination, &bytes)
            .map_err(|error| format!("cannot write {}: {error}", destination.display()))?;
        println!(
            "{}: {} directories, {} files indexed (index object {sha256}, {} bytes)",
            wim.display(),
            index.directories.len(),
            index.files.len(),
            bytes.len()
        );
        Ok(ObjectRef {
            sha256,
            size: bytes.len() as u64,
        })
    })();
    let _ = fs::remove_dir_all(&extract_root);
    result
}

fn generate(
    root: &Path,
    changelog_path: &Path,
    version: &str,
    build_epoch: &str,
    objects: &Path,
) -> Result<(), String> {
    validate_version(version)?;
    let build_epoch: u64 = build_epoch
        .parse()
        .map_err(|_| format!("invalid build epoch {build_epoch:?}"))?;
    let changelog = read_changelog(changelog_path)?;
    if changelog.entries.is_empty() {
        println!("::warning::PENDING_CHANGELOG.json has no entries; the update prompt will show no changelog");
    }
    fs::create_dir_all(objects)
        .map_err(|error| format!("cannot create {}: {error}", objects.display()))?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.map_err(|error| format!("cannot walk {}: {error}", root.display()))?;
        if entry.path_is_symlink() || entry.file_type().is_symlink() {
            return Err(format!("{} is a link", entry.path().display()));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = relative_slash_path(root, entry.path())?;
        if relative.eq_ignore_ascii_case("config.json")
            || relative.eq_ignore_ascii_case(RELEASE_MANIFEST_PATH)
        {
            continue;
        }
        validate_package_path(&relative)?;
        let (size, sha256) = store_object(entry.path(), objects)?;
        let mut file = PackageFile {
            path: relative,
            size,
            sha256,
            wim_index: None,
        };
        if is_user_managed_image(&file.path) {
            match build_image_index(entry.path(), objects) {
                Ok(index) => file.wim_index = Some(index),
                Err(error) => println!(
                    "::warning::{} gets no per-file delta and will be downloaded whole: {error}",
                    file.path
                ),
            }
        }
        files.push(file);
    }
    let manifest = UpdateManifest {
        schema_version: SCHEMA_VERSION,
        product: PRODUCT.to_owned(),
        version: version.to_owned(),
        build_epoch,
        changelog: changelog.entries,
        files,
    };
    manifest.validate()?;
    let manifest_path = RELEASE_MANIFEST_PATH
        .split('/')
        .fold(root.to_path_buf(), |path, component| path.join(component));
    write_text(&manifest_path, &manifest.to_pretty_json())?;
    let reread = fs::read(&manifest_path)
        .map_err(|error| format!("cannot re-read {}: {error}", manifest_path.display()))?;
    if UpdateManifest::parse(&reread)? != manifest {
        return Err("release manifest changed during the read-back check".to_owned());
    }
    let object_count = fs::read_dir(objects)
        .map_err(|error| format!("cannot list {}: {error}", objects.display()))?
        .count();
    println!(
        "{}: {} package files, version {version}, build epoch {build_epoch}, {object_count} objects in {}",
        manifest_path.display(),
        manifest.files.len(),
        objects.display()
    );
    Ok(())
}
