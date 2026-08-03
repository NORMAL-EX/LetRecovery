//! Verification and hardware matching for bundled Windows 7 USB3/NVMe payloads.
//!
//! The bytes originate from the reviewed legacy package, but are accepted only
//! through the embedded SHA-256 lock. USB3 packages are additionally selected
//! by present SetupAPI hardware IDs and target architecture. The NVMe payload is
//! the ordered Microsoft x64 hotfix pair and is never offered to x86 images.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use walkdir::WalkDir;

const LOCK_JSON: &str = include_str!("../../docs/WINDOWS7_DRIVERS.lock.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Windows7TargetArchitecture {
    X86,
    Amd64,
}

impl Windows7TargetArchitecture {
    const fn lock_name(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::Amd64 => "amd64",
        }
    }
}

#[derive(Debug, Deserialize)]
struct DriverLock {
    version: u32,
    usb3_packages: Vec<Usb3PackageLock>,
    nvme: NvmeLock,
}

#[derive(Debug, Deserialize)]
struct Usb3PackageLock {
    directory: String,
    architectures: Vec<String>,
    files: Vec<LockedFile>,
}

#[derive(Debug, Deserialize)]
struct NvmeLock {
    architecture: String,
    install_order: Vec<String>,
    files: Vec<LockedFile>,
}

#[derive(Debug, Deserialize)]
struct LockedFile {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug)]
struct VerifiedUsb3Package {
    directory: PathBuf,
    architectures: Vec<String>,
}

#[derive(Debug)]
pub struct VerifiedWindows7DriverPayload {
    usb3_packages: Vec<VerifiedUsb3Package>,
    nvme_architecture: String,
    nvme_cabs: Vec<PathBuf>,
}

impl VerifiedWindows7DriverPayload {
    /// Selects only packages that support the applied image architecture and
    /// contain an INF matching at least one hardware ID reported by SetupAPI.
    pub fn select_usb3_packages(
        &self,
        hardware_ids: &[String],
        architecture: Windows7TargetArchitecture,
    ) -> Result<Vec<PathBuf>> {
        let mut selected = Vec::new();
        for package in &self.usb3_packages {
            if !package
                .architectures
                .iter()
                .any(|value| value == architecture.lock_name())
            {
                continue;
            }
            if crate::storage_driver_match::inf_tree_matches_any_hardware_id(
                &package.directory,
                hardware_ids,
            )? {
                selected.push(package.directory.clone());
            }
        }
        Ok(selected)
    }

    pub fn nvme_cabs(&self, architecture: Windows7TargetArchitecture) -> Result<&[PathBuf]> {
        if self.nvme_architecture != architecture.lock_name() {
            bail!("bundled Windows 7 NVMe updates do not support the target architecture");
        }
        Ok(&self.nvme_cabs)
    }
}

/// Verifies every bundled byte and rejects missing, extra, linked or renamed
/// files before DISM receives a path.
pub fn verify_windows7_driver_payload(
    drivers_root: &Path,
) -> Result<VerifiedWindows7DriverPayload> {
    let lock: DriverLock =
        serde_json::from_str(LOCK_JSON).context("parse embedded Windows 7 driver lock manifest")?;
    if lock.version != 1 || lock.usb3_packages.is_empty() {
        bail!("unsupported Windows 7 driver lock manifest");
    }

    let usb3_root = drivers_root.join("usb3");
    require_regular_directory(&usb3_root)?;
    let mut expected_package_names = BTreeSet::new();
    let mut usb3_packages = Vec::with_capacity(lock.usb3_packages.len());
    for package in lock.usb3_packages {
        validate_single_component(&package.directory)?;
        let folded = package.directory.to_ascii_lowercase();
        if !expected_package_names.insert(folded) {
            bail!("duplicate Windows 7 USB3 package directory in lock manifest");
        }
        if package.architectures.is_empty()
            || package
                .architectures
                .iter()
                .any(|value| !matches!(value.as_str(), "x86" | "amd64"))
        {
            bail!("invalid Windows 7 USB3 package architecture set");
        }
        let directory = usb3_root.join(&package.directory);
        verify_tree(&directory, &package.files)?;
        usb3_packages.push(VerifiedUsb3Package {
            directory,
            architectures: package.architectures,
        });
    }
    let actual_package_names = std::fs::read_dir(&usb3_root)
        .with_context(|| format!("enumerate Windows 7 USB3 root: {}", usb3_root.display()))?
        .map(|entry| {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                bail!(
                    "unexpected non-directory in Windows 7 USB3 root: {}",
                    entry.path().display()
                );
            }
            Ok(entry.file_name().to_string_lossy().to_ascii_lowercase())
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if actual_package_names != expected_package_names {
        bail!("Windows 7 USB3 package membership does not match the lock manifest");
    }

    if lock.nvme.architecture != "amd64" {
        bail!("Windows 7 NVMe lock must remain amd64-only");
    }
    let nvme_root = drivers_root.join("nvme");
    verify_tree(&nvme_root, &lock.nvme.files)?;
    let locked_names = lock
        .nvme
        .files
        .iter()
        .map(|file| file.path.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let ordered_names = lock
        .nvme
        .install_order
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if lock.nvme.install_order.len() != 2 || ordered_names != locked_names {
        bail!("Windows 7 NVMe install order does not match the locked CAB set");
    }
    let nvme_cabs = lock
        .nvme
        .install_order
        .iter()
        .map(|name| {
            validate_single_component(name)?;
            Ok(nvme_root.join(name))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(VerifiedWindows7DriverPayload {
        usb3_packages,
        nvme_architecture: lock.nvme.architecture,
        nvme_cabs,
    })
}

fn verify_tree(root: &Path, locked_files: &[LockedFile]) -> Result<()> {
    require_regular_directory(root)?;
    if locked_files.is_empty() {
        bail!("Windows 7 driver package lock is empty: {}", root.display());
    }
    let mut expected = BTreeSet::new();
    for locked in locked_files {
        let relative = validate_relative_path(&locked.path)?;
        let folded = normalized_relative(&relative);
        if !expected.insert(folded) {
            bail!(
                "duplicate Windows 7 driver file in lock manifest: {}",
                locked.path
            );
        }
        let path = root.join(relative);
        let metadata = path.symlink_metadata().with_context(|| {
            format!(
                "locked Windows 7 driver file is unavailable: {}",
                path.display()
            )
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!(
                "locked Windows 7 driver member is not a regular file: {}",
                path.display()
            );
        }
        if metadata.len() != locked.size {
            bail!(
                "locked Windows 7 driver member size mismatch: {}",
                path.display()
            );
        }
        let actual = crate::hash::sha256_file(&path, |_| {})
            .with_context(|| format!("hash locked Windows 7 driver member: {}", path.display()))?;
        if !actual.eq_ignore_ascii_case(&locked.sha256) {
            bail!(
                "locked Windows 7 driver member SHA-256 mismatch: {}",
                path.display()
            );
        }
    }

    let mut actual = BTreeSet::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry
            .with_context(|| format!("enumerate Windows 7 driver tree: {}", root.display()))?;
        if entry.path() == root {
            continue;
        }
        if entry.file_type().is_symlink() {
            bail!(
                "Windows 7 driver tree contains a link: {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(root)?;
            actual.insert(normalized_relative(relative));
        }
    }
    if actual != expected {
        bail!(
            "Windows 7 driver tree membership does not match the lock manifest: {}",
            root.display()
        );
    }
    Ok(())
}

fn require_regular_directory(path: &Path) -> Result<()> {
    let metadata = path.symlink_metadata().with_context(|| {
        format!(
            "Windows 7 driver directory is unavailable: {}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "Windows 7 driver directory is not regular: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_single_component(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        bail!("invalid Windows 7 driver package name: {value}");
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<PathBuf> {
    let normalized = value.replace('/', "\\");
    let path = PathBuf::from(normalized);
    if path.is_absolute()
        || path.components().next().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("invalid locked Windows 7 driver path: {value}");
    }
    Ok(path)
}

fn normalized_relative(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_lock_has_expected_shape_and_order() {
        let lock: DriverLock = serde_json::from_str(LOCK_JSON).unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.usb3_packages.len(), 13);
        assert_eq!(
            lock.nvme.install_order,
            [
                "Windows6.1-KB2990941-v3-x64.cab",
                "Windows6.1-KB3087873-v2-x64.cab"
            ]
        );
    }

    #[test]
    fn traversal_and_absolute_lock_paths_are_rejected() {
        for value in ["../evil.sys", r"C:\evil.sys", "/evil.sys", ""] {
            assert!(validate_relative_path(value).is_err());
        }
        assert!(validate_relative_path("x64/driver.sys").is_ok());
    }
}
