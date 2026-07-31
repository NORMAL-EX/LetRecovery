//! Selects and verifies built-in storage-controller driver packages.
//!
//! Storage miniport drivers are boot-critical. Selection is based only on present PCI hardware
//! IDs reported by SetupAPI. Package bytes are pinned so a base package or writable release tree
//! cannot silently replace a driver before `drvload` or offline DISM sees it.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltInStorageDriverPackage {
    /// Intel VMD 20.2.4.1019, retained for the 11th-generation 9A0B controller.
    IntelVmd11th,
    /// Intel VMD 20.2.12.1036 for later 467F/A77F/7D0B/AD0B controllers.
    IntelVmdCurrent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageDriverSelectionError {
    message: &'static str,
}

impl fmt::Display for StorageDriverSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for StorageDriverSelectionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedStorageDriverPackage {
    package: BuiltInStorageDriverPackage,
    directory: PathBuf,
    inf_path: PathBuf,
}

impl VerifiedStorageDriverPackage {
    pub fn package(&self) -> BuiltInStorageDriverPackage {
        self.package
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn inf_path(&self) -> &Path {
        &self.inf_path
    }
}

#[derive(Clone, Copy)]
struct LockedPackageFile {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

const INTEL_VMD_11TH: &str = "PCI\\VEN_8086&DEV_9A0B";
const INTEL_VMD_MANAGED: &str = "PCI\\VEN_8086&DEV_09AB";
const INTEL_VMD_CURRENT: [&str; 4] = [
    "PCI\\VEN_8086&DEV_467F",
    "PCI\\VEN_8086&DEV_A77F",
    "PCI\\VEN_8086&DEV_7D0B",
    "PCI\\VEN_8086&DEV_AD0B",
];

const INTEL_VMD_11TH_FILES: [LockedPackageFile; 6] = [
    LockedPackageFile {
        name: "iaStorVD.cat",
        size: 12_670,
        sha256: "7B5494A139F756AEEFCBFFFCC27F190625F629CF49D57EF15A198D687586F039",
    },
    LockedPackageFile {
        name: "iaStorVD.inf",
        size: 28_852,
        sha256: "7186258DDE0C9B4F5C78C93B571C5314394E01EE376BDE6D89370A88DF246038",
    },
    LockedPackageFile {
        name: "iaStorVD.sys",
        size: 1_623_632,
        sha256: "3DD035A3669B735B707BE0FC2BA6840675090337062758D734458CEB4B55EEF2",
    },
    LockedPackageFile {
        name: "NOTICE.txt",
        size: 1_290,
        sha256: "B2295BBE4F544BA60B3C1651FD67145821B4A4D25814F262BA993AACDEA3CF7C",
    },
    LockedPackageFile {
        name: "RstMwEventLogMsg.dll",
        size: 30_800,
        sha256: "E4E91DA33405DED01DC716D1C1E7BA0842FF0A244617F62078E3E3267C45E797",
    },
    LockedPackageFile {
        name: "RstMwService.exe",
        size: 2_065_488,
        sha256: "064A6B48B0CB5FE5E5ADF3FE66D7570C20C2EACF662D36A3943B37BDC406BCE3",
    },
];

const INTEL_VMD_CURRENT_FILES: [LockedPackageFile; 6] = [
    LockedPackageFile {
        name: "iaStorVD.cat",
        size: 12_577,
        sha256: "CF1CCDDF06D502162FCED96F11A0664AB36EA1B167CB795D14F410D4737DF2AB",
    },
    LockedPackageFile {
        name: "iaStorVD.inf",
        size: 28_570,
        sha256: "FDE7E9089C5EAFDA7AD6B3CEA83F46F65DAE21EDE6EA50960FE7A923AD4A80A7",
    },
    LockedPackageFile {
        name: "iaStorVD.sys",
        size: 1_624_192,
        sha256: "0D4017CB74827B6FD39365628829F54C9CF675891E6A511F2A23A2ACE2CA9B8A",
    },
    LockedPackageFile {
        name: "NOTICE.txt",
        size: 1_307,
        sha256: "FE8622638D08EF8572C72FDBE428C328AB06B351160EA2CBD3D524405BEC8B4A",
    },
    LockedPackageFile {
        name: "RstMwEventLogMsg.dll",
        size: 30_848,
        sha256: "A3DE1A8FB790BFC3E7BF126EC6AEC20C56C165BFC49AF55B93A84E12236B3585",
    },
    LockedPackageFile {
        name: "RstMwService.exe",
        size: 2_066_560,
        sha256: "826CCC62ECFE4121DF5DB73382FD32C57384ABBF8B6280AD0F6A5C54AF87485D",
    },
];

impl BuiltInStorageDriverPackage {
    pub const fn directory_name(self) -> &'static str {
        match self {
            Self::IntelVmd11th => "intel-vmd-11th",
            Self::IntelVmdCurrent => "intel-vmd-current",
        }
    }

    pub const fn controller_hardware_ids(self) -> &'static [&'static str] {
        match self {
            Self::IntelVmd11th => &[INTEL_VMD_11TH],
            Self::IntelVmdCurrent => &INTEL_VMD_CURRENT,
        }
    }

    const fn locked_files(self) -> &'static [LockedPackageFile] {
        match self {
            Self::IntelVmd11th => &INTEL_VMD_11TH_FILES,
            Self::IntelVmdCurrent => &INTEL_VMD_CURRENT_FILES,
        }
    }
}

fn contains_device_id(hardware_id: &str, device_id: &str) -> bool {
    let normalized = hardware_id.trim().to_ascii_uppercase();
    normalized == device_id
        || normalized
            .strip_prefix(device_id)
            .is_some_and(|suffix| suffix.starts_with('&'))
}

/// Returns only packages whose generation-defining controller is present.
///
/// `09AB` is a managed/dummy VMD function and cannot identify a processor generation. Seeing it
/// alone is therefore an error, not permission to guess a legacy package. AMD, Apple, VirtIO and
/// unrelated Intel IDs intentionally select nothing.
pub fn select_builtin_storage_driver_packages<'a>(
    hardware_ids: impl IntoIterator<Item = &'a str>,
) -> std::result::Result<Vec<BuiltInStorageDriverPackage>, StorageDriverSelectionError> {
    let ids: Vec<&str> = hardware_ids.into_iter().collect();
    let has_11th = ids.iter().any(|id| contains_device_id(id, INTEL_VMD_11TH));
    let has_current = ids.iter().any(|id| {
        INTEL_VMD_CURRENT
            .iter()
            .any(|device_id| contains_device_id(id, device_id))
    });
    let has_managed = ids
        .iter()
        .any(|id| contains_device_id(id, INTEL_VMD_MANAGED));

    if has_managed && !has_11th && !has_current {
        return Err(StorageDriverSelectionError {
            message: "Intel VMD managed function 09AB is present without a generation-defining controller",
        });
    }

    let mut selected = Vec::with_capacity(2);
    if has_11th {
        selected.push(BuiltInStorageDriverPackage::IntelVmd11th);
    }
    if has_current {
        selected.push(BuiltInStorageDriverPackage::IntelVmdCurrent);
    }
    Ok(selected)
}

/// Verifies every byte of one bundled package before it crosses a boot-critical boundary.
pub fn verify_builtin_storage_driver_package(
    package: BuiltInStorageDriverPackage,
    directory: &Path,
) -> Result<VerifiedStorageDriverPackage> {
    let directory_metadata = directory.symlink_metadata().with_context(|| {
        format!(
            "storage driver package is unavailable: {}",
            directory.display()
        )
    })?;
    if !directory_metadata.file_type().is_dir() || directory_metadata.file_type().is_symlink() {
        bail!(
            "storage driver package is not a regular directory: {}",
            directory.display()
        );
    }

    let expected_names = package
        .locked_files()
        .iter()
        .map(|file| file.name.to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    let mut actual_names = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(directory).with_context(|| {
        format!(
            "failed to enumerate storage driver package: {}",
            directory.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to enumerate storage driver package: {}",
                directory.display()
            )
        })?;
        let file_type = entry.file_type().with_context(|| {
            format!(
                "failed to inspect storage driver package member: {}",
                entry.path().display()
            )
        })?;
        if !file_type.is_file() || file_type.is_symlink() {
            bail!(
                "storage driver package contains a non-file member: {}",
                entry.path().display()
            );
        }
        actual_names.insert(entry.file_name().to_string_lossy().to_ascii_lowercase());
    }
    if actual_names != expected_names {
        bail!(
            "storage driver package membership does not match the locked manifest: {}",
            directory.display()
        );
    }

    for locked in package.locked_files() {
        let path = directory.join(locked.name);
        let metadata = path.symlink_metadata().with_context(|| {
            format!(
                "storage driver package file is unavailable: {}",
                path.display()
            )
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!(
                "storage driver package member is not a regular file: {}",
                path.display()
            );
        }
        if metadata.len() != locked.size {
            bail!(
                "storage driver package member size mismatch: {} ({} != {})",
                path.display(),
                metadata.len(),
                locked.size
            );
        }
        let actual = crate::hash::sha256_file(&path, |_| {}).with_context(|| {
            format!(
                "failed to hash storage driver package member: {}",
                path.display()
            )
        })?;
        if !actual.eq_ignore_ascii_case(locked.sha256) {
            bail!(
                "storage driver package member SHA-256 mismatch: {}",
                path.display()
            );
        }
    }

    let inf_path = directory.join("iaStorVD.inf");
    let inf_text = read_inf_text(&inf_path)?;
    if !package
        .controller_hardware_ids()
        .iter()
        .all(|hardware_id| inf_contains_hardware_id(&inf_text, hardware_id))
    {
        bail!(
            "storage driver INF does not cover its locked controller set: {}",
            inf_path.display()
        );
    }

    Ok(VerifiedStorageDriverPackage {
        package,
        directory: directory.to_path_buf(),
        inf_path,
    })
}

/// Returns whether at least one regular INF below `root` covers `hardware_id`.
pub fn inf_tree_contains_hardware_id(root: &Path, hardware_id: &str) -> Result<bool> {
    let metadata = root
        .symlink_metadata()
        .with_context(|| format!("driver tree is unavailable: {}", root.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!("driver tree is not a regular directory: {}", root.display());
    }

    let mut inspected = 0usize;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry
            .with_context(|| format!("failed to enumerate driver tree: {}", root.display()))?;
        if !entry.file_type().is_file()
            || !entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
        {
            continue;
        }
        inspected += 1;
        if inspected > 16_384 {
            bail!(
                "driver tree contains too many INF files: {}",
                root.display()
            );
        }
        let text = read_inf_text(entry.path())?;
        if inf_contains_hardware_id(&text, hardware_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_inf_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read driver INF: {}", path.display()))?;
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let words = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16_lossy(&words))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        let words = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16_lossy(&words))
    } else {
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn inf_contains_hardware_id(inf_text: &str, hardware_id: &str) -> bool {
    let inf = inf_text.to_ascii_uppercase();
    hardware_id_match_keys(hardware_id)
        .iter()
        .any(|key| contains_hardware_id_key(&inf, key))
}

fn contains_hardware_id_key(inf: &str, key: &str) -> bool {
    let mut offset = 0usize;
    while let Some(relative) = inf[offset..].find(key) {
        let start = offset + relative;
        let end = start + key.len();
        let before_ok = start == 0 || !inf.as_bytes()[start - 1].is_ascii_alphanumeric();
        let after_ok = end == inf.len()
            || matches!(
                inf.as_bytes()[end],
                b'&' | b'"' | b'\'' | b',' | b';' | b' ' | b'\t' | b'\r' | b'\n'
            );
        if before_ok && after_ok {
            return true;
        }
        offset = end;
    }
    false
}

fn hardware_id_match_keys(hardware_id: &str) -> Vec<String> {
    let normalized = hardware_id.trim().to_ascii_uppercase();
    let mut keys = vec![normalized.clone()];
    if let Some(device_pos) = normalized.find("&DEV_") {
        let end = (device_pos + 9).min(normalized.len());
        let base = normalized[..end].to_string();
        if base != normalized {
            keys.push(base);
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_11th_generation_vmd_by_exact_vendor_and_device() {
        assert_eq!(
            select_builtin_storage_driver_packages([
                "PCI\\VEN_8086&DEV_9A0B&SUBSYS_00000000",
                "PCI\\VEN_8086&DEV_09AB",
            ])
            .unwrap(),
            vec![BuiltInStorageDriverPackage::IntelVmd11th]
        );
    }

    #[test]
    fn selects_current_vmd_without_also_staging_legacy_for_managed_function() {
        assert_eq!(
            select_builtin_storage_driver_packages([
                "pci\\ven_8086&dev_467f&cc_0104",
                "PCI\\VEN_8086&DEV_09AB",
            ])
            .unwrap(),
            vec![BuiltInStorageDriverPackage::IntelVmdCurrent]
        );
    }

    #[test]
    fn managed_function_alone_is_ambiguous() {
        let error = select_builtin_storage_driver_packages(["PCI\\VEN_8086&DEV_09AB"]).unwrap_err();
        assert!(error.to_string().contains("09AB"));
    }

    #[test]
    fn amd_virtio_apple_and_similar_prefixes_select_nothing() {
        for hardware_id in [
            "PCI\\VEN_1022&DEV_43BD",
            "PCI\\VEN_1AF4&DEV_1001",
            "PCI\\VEN_106B&DEV_2001",
            "PCI\\VEN_8086&DEV_9A0C",
            "PCI\\VEN_8086&DEV_467F0",
        ] {
            assert!(select_builtin_storage_driver_packages([hardware_id])
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn inf_matching_accepts_a_broader_vendor_device_model() {
        let inf = "%Device%=Install, PCI\\VEN_8086&DEV_A77F\r\n";
        assert!(inf_contains_hardware_id(
            inf,
            "PCI\\VEN_8086&DEV_A77F&SUBSYS_12341043&REV_01"
        ));
        assert!(!inf_contains_hardware_id(
            "%Device%=Install, PCI\\VEN_8086&DEV_A77F0\r\n",
            "PCI\\VEN_8086&DEV_A77F&SUBSYS_12341043"
        ));
    }
}
