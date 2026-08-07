//! Offline PCA2023 compatibility assets for supported Windows images.
//!
//! LetRecovery ships a small, fixed set of WIM resource packs. Selection is
//! based on the target image architecture and boot-environment family; no
//! network access is required. Every package is signature-checked before use
//! and SHA-256 binds a package staged by the desktop endpoint to WinPE.

use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::boot_pca::{inspect_efi_architecture, inspect_efi_embedded_signer, PcaGeneration};
use crate::hash::{hash_matches, normalize_hash, sha256_file};
use crate::pca_preflight::inspect_wim_boot_source_details;
use crate::scoped_temp_file::ScopedTempDir;
use crate::wimlib::WimlibManager;

pub const STAGED_PACKAGE_RELATIVE_PATH: &str = "pca_compat\\package.wim";

const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const PACKAGE_IMAGE_INDEX: u32 = 1;
const MODERN_BOOT_FAMILY_MIN_BUILD: u32 = 26_100;
const WINDOWS_11_MIN_BUILD: u32 = 22_000;

const LEGACY_AMD64_PACKAGE: &str = "pca2023-legacy-amd64.wim";
const LEGACY_X86_PACKAGE: &str = "pca2023-windows10-x86.wim";
const MODERN_AMD64_PACKAGE: &str = "pca2023-modern-amd64.wim";

const BOOTEX_BOOT_MANAGER: &str = "\\Windows\\Boot\\EFI_EX\\bootmgfw_EX.efi";
const BOOTEX_FONTS: &str = "\\Windows\\Boot\\FONTS_EX";
const BOOT_STL: &str = "\\Windows\\Boot\\EFI\\boot.stl";
const REQUIRED_INJECTION_PATHS: [&str; 2] = ["\\Windows\\Boot\\EFI_EX", BOOTEX_FONTS];
const PCA2023_RESOURCE_LOCK: &str = include_str!("../../docs/PCA2023_RESOURCES.lock.json");

#[derive(Debug, Deserialize)]
struct PcaResourceLock {
    schema_version: u32,
    packages: Vec<LockedPcaPackage>,
}

#[derive(Debug, Clone, Deserialize)]
struct LockedPcaPackage {
    file: String,
    target_wim_architecture: u16,
    size: u64,
    sha256: String,
    bootmgfw_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetImageIdentity {
    pub build: u32,
    pub architecture: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcaCompatFamily {
    Windows10AndServer2016Plus,
    Windows11Modern,
}

impl fmt::Display for PcaCompatFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows10AndServer2016Plus => f.write_str("Windows 10 / Server 2016+"),
            Self::Windows11Modern => f.write_str("Windows 11 24H2+ / Server 2025+"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfflineAssetSelection {
    pub family: PcaCompatFamily,
    pub file_name: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PcaCompatError {
    ImageMetadata(String),
    UnsupportedTarget(TargetImageIdentity),
    MissingOfflineAsset(PathBuf),
    PackageTooLarge(u64),
    PackageIntegrity(String),
    InvalidPackage(String),
    Io(String),
}

impl fmt::Display for PcaCompatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImageMetadata(error) => write!(f, "无法识别目标镜像版本或架构: {error}"),
            Self::UnsupportedTarget(target) => write!(
                f,
                "不支持为 Windows build {} / architecture {} 准备 PCA2023 离线资源",
                target.build, target.architecture
            ),
            Self::MissingOfflineAsset(path) => {
                write!(f, "缺少 PCA2023 离线资源包: {}", path.display())
            }
            Self::PackageTooLarge(size) => {
                write!(f, "PCA2023 离线资源包超过大小上限: {size} bytes")
            }
            Self::PackageIntegrity(error) => {
                write!(f, "PCA2023 离线资源包完整性校验失败: {error}")
            }
            Self::InvalidPackage(error) => write!(f, "PCA2023 离线资源包无效: {error}"),
            Self::Io(error) => write!(f, "PCA2023 离线资源文件操作失败: {error}"),
        }
    }
}

impl std::error::Error for PcaCompatError {}

#[derive(Debug)]
pub struct PreparedPcaCompatPackage {
    path: PathBuf,
    sha256: String,
    image_index: u32,
    target: TargetImageIdentity,
    family: PcaCompatFamily,
    locked_file_name: &'static str,
}

impl PreparedPcaCompatPackage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub const fn image_index(&self) -> u32 {
        self.image_index
    }

    pub const fn target(&self) -> TargetImageIdentity {
        self.target
    }

    pub const fn family(&self) -> PcaCompatFamily {
        self.family
    }

    /// Persist a verified package beside the PE task configuration. A second
    /// hash pass detects copy corruption before the original is replaced.
    pub fn persist_to(&self, destination: &Path) -> Result<(), PcaCompatError> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| PcaCompatError::Io(error.to_string()))?;
        }
        let temporary = destination.with_extension("wim.part");
        let backup = destination.with_extension("wim.bak");
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&backup);
        fs::copy(&self.path, &temporary).map_err(|error| PcaCompatError::Io(error.to_string()))?;
        verify_sha256_file(&temporary, &self.sha256)?;

        if destination.exists() {
            fs::rename(destination, &backup)
                .map_err(|error| PcaCompatError::Io(error.to_string()))?;
        }
        if let Err(error) = fs::rename(&temporary, destination) {
            if backup.exists() {
                let _ = fs::rename(&backup, destination);
            }
            let _ = fs::remove_file(&temporary);
            return Err(PcaCompatError::Io(error.to_string()));
        }
        let _ = fs::remove_file(&backup);
        Ok(())
    }

    /// Inject only the fixed BootEx resource directories into an already
    /// applied Windows image. No scripts, BCD store, registry data, or ESP
    /// content can be supplied by the resource pack.
    pub fn inject_into_offline_windows(&self, target_root: &Path) -> Result<(), PcaCompatError> {
        let selection = select_offline_asset(self.target)?;
        if selection.file_name != self.locked_file_name {
            return Err(PcaCompatError::InvalidPackage(
                "prepared package identity no longer matches its target image".to_string(),
            ));
        }
        let locked = locked_package(selection.file_name)?;
        if !hash_matches(&self.sha256, &locked.sha256) {
            return Err(PcaCompatError::PackageIntegrity(
                "prepared package SHA-256 does not match the embedded release lock".to_string(),
            ));
        }
        validate_locked_package(
            &self.path,
            self.image_index,
            self.target.architecture,
            &locked,
        )?;

        let target = target_root.to_string_lossy();
        let package = self.path.to_string_lossy();
        let manager = WimlibManager::new().map_err(PcaCompatError::InvalidPackage)?;
        let mut injection_paths = REQUIRED_INJECTION_PATHS.to_vec();
        if manager
            .image_contains_path(&package, self.image_index, BOOT_STL)
            .map_err(PcaCompatError::InvalidPackage)?
        {
            injection_paths.push(BOOT_STL);
        }
        manager
            .extract_paths(&package, self.image_index, &target, &injection_paths)
            .map_err(PcaCompatError::InvalidPackage)?;

        let windows = target_root.join("Windows");
        validate_locked_bootex(
            &windows.join("Boot").join("EFI_EX").join("bootmgfw_EX.efi"),
            self.target.architecture,
            &locked,
        )?;
        for relative in ["Boot\\FONTS_EX"] {
            if !windows.join(relative).exists() {
                return Err(PcaCompatError::InvalidPackage(format!(
                    "注入后缺少必需资源: Windows\\{relative}"
                )));
            }
        }
        Ok(())
    }
}

pub fn target_image_identity(
    image_file: &Path,
    image_index: u32,
) -> Result<TargetImageIdentity, PcaCompatError> {
    let (_, identity) = target_image_metadata(image_file, image_index)?;
    Ok(identity)
}

fn target_image_metadata(
    image_file: &Path,
    image_index: u32,
) -> Result<(u16, TargetImageIdentity), PcaCompatError> {
    let path = image_file
        .to_str()
        .ok_or_else(|| PcaCompatError::ImageMetadata("镜像路径不是有效 Unicode".to_string()))?;
    let images = WimlibManager::new()
        .map_err(PcaCompatError::ImageMetadata)?
        .get_image_info(path)
        .map_err(PcaCompatError::ImageMetadata)?;
    let image = images
        .iter()
        .find(|image| image.index == image_index)
        .ok_or_else(|| PcaCompatError::ImageMetadata(format!("镜像卷索引 {image_index} 不存在")))?;
    let major = image
        .major_version
        .ok_or_else(|| PcaCompatError::ImageMetadata("WIM XML 缺少 VERSION/MAJOR".to_string()))?;
    let build = image
        .build
        .ok_or_else(|| PcaCompatError::ImageMetadata("WIM XML 缺少 VERSION/BUILD".to_string()))?;
    let architecture = image
        .architecture
        .ok_or_else(|| PcaCompatError::ImageMetadata("WIM XML 缺少 WINDOWS/ARCH".to_string()))?;
    Ok((
        major,
        TargetImageIdentity {
            build,
            architecture,
        },
    ))
}

pub fn select_offline_asset(
    target: TargetImageIdentity,
) -> Result<OfflineAssetSelection, PcaCompatError> {
    match target.architecture {
        9 if target.build >= MODERN_BOOT_FAMILY_MIN_BUILD => Ok(OfflineAssetSelection {
            family: PcaCompatFamily::Windows11Modern,
            file_name: MODERN_AMD64_PACKAGE,
        }),
        9 => Ok(OfflineAssetSelection {
            family: PcaCompatFamily::Windows10AndServer2016Plus,
            file_name: LEGACY_AMD64_PACKAGE,
        }),
        0 if target.build < WINDOWS_11_MIN_BUILD => Ok(OfflineAssetSelection {
            family: PcaCompatFamily::Windows10AndServer2016Plus,
            file_name: LEGACY_X86_PACKAGE,
        }),
        _ => Err(PcaCompatError::UnsupportedTarget(target)),
    }
}

/// Select and validate a bundled package. This function performs no network
/// request and does not touch the target disk.
pub fn prepare_from_local_assets(
    image_file: &Path,
    image_index: u32,
    asset_directory: &Path,
) -> Result<PreparedPcaCompatPackage, PcaCompatError> {
    let (major, target) = target_image_metadata(image_file, image_index)?;
    if major != 10 {
        return Err(PcaCompatError::UnsupportedTarget(target));
    }
    let selection = select_offline_asset(target)?;
    let package_path = asset_directory.join(selection.file_name);
    let locked = locked_package(selection.file_name)?;
    validate_locked_package(
        &package_path,
        PACKAGE_IMAGE_INDEX,
        target.architecture,
        &locked,
    )?;

    Ok(PreparedPcaCompatPackage {
        path: package_path,
        sha256: normalize_hash(&locked.sha256),
        image_index: PACKAGE_IMAGE_INDEX,
        target,
        family: selection.family,
        locked_file_name: selection.file_name,
    })
}

/// Validate a PCA2023 resource WIM without applying it to a Windows image.
///
/// `target_architecture` uses WIM XML values (`0` for x86 and `9` for amd64),
/// not PE COFF machine constants.
pub fn validate_offline_asset_package(
    package_path: &Path,
    target_architecture: u16,
) -> Result<(), PcaCompatError> {
    validate_local_package_file(package_path)?;
    validate_package_wim(package_path, PACKAGE_IMAGE_INDEX, target_architecture)
}

pub fn open_staged_package(
    image_file: &Path,
    image_index: u32,
    package_path: &Path,
    expected_sha256: &str,
    package_image_index: u32,
    expected_target: TargetImageIdentity,
) -> Result<PreparedPcaCompatPackage, PcaCompatError> {
    validate_local_package_file(package_path)?;
    let actual_target = target_image_identity(image_file, image_index)?;
    if actual_target != expected_target {
        return Err(PcaCompatError::InvalidPackage(format!(
            "暂存包目标不匹配：配置 {:?}，镜像 {:?}",
            expected_target, actual_target
        )));
    }
    let selection = select_offline_asset(actual_target)?;
    let locked = locked_package(selection.file_name)?;
    if !hash_matches(expected_sha256, &locked.sha256) {
        return Err(PcaCompatError::PackageIntegrity(
            "staged package SHA-256 does not match the embedded release lock".to_string(),
        ));
    }
    validate_locked_package(
        package_path,
        package_image_index,
        actual_target.architecture,
        &locked,
    )?;
    Ok(PreparedPcaCompatPackage {
        path: package_path.to_path_buf(),
        sha256: normalize_hash(&locked.sha256),
        image_index: package_image_index,
        target: actual_target,
        family: selection.family,
        locked_file_name: selection.file_name,
    })
}

/// Resolve a config-provided package path below the PE data directory without
/// allowing absolute paths, parent traversal, prefixes, or non-WIM files.
pub fn resolve_staged_package_path(
    data_directory: &Path,
    relative_path: &str,
) -> Result<PathBuf, PcaCompatError> {
    if relative_path.trim().is_empty() || relative_path.len() > 240 {
        return Err(PcaCompatError::InvalidPackage(
            "暂存资源包相对路径为空或过长".to_string(),
        ));
    }
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PcaCompatError::InvalidPackage(
            "暂存资源包必须是数据目录内的安全相对路径".to_string(),
        ));
    }
    if !relative
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wim"))
    {
        return Err(PcaCompatError::InvalidPackage(
            "暂存资源包必须使用 .wim 扩展名".to_string(),
        ));
    }
    Ok(data_directory.join(relative))
}

fn locked_package(file_name: &str) -> Result<LockedPcaPackage, PcaCompatError> {
    let resource_lock: PcaResourceLock =
        serde_json::from_str(PCA2023_RESOURCE_LOCK).map_err(|error| {
            PcaCompatError::InvalidPackage(format!(
                "embedded PCA2023 release lock is invalid: {error}"
            ))
        })?;
    if resource_lock.schema_version != 1 {
        return Err(PcaCompatError::InvalidPackage(format!(
            "unsupported PCA2023 release lock schema {}",
            resource_lock.schema_version
        )));
    }
    if resource_lock.packages.len() != 3 {
        return Err(PcaCompatError::InvalidPackage(format!(
            "PCA2023 release lock must contain exactly three packages, found {}",
            resource_lock.packages.len()
        )));
    }

    let expected = [
        (LEGACY_AMD64_PACKAGE, 9u16),
        (LEGACY_X86_PACKAGE, 0u16),
        (MODERN_AMD64_PACKAGE, 9u16),
    ];
    for (expected_name, expected_architecture) in expected {
        let matches = resource_lock
            .packages
            .iter()
            .filter(|package| package.file == expected_name)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(PcaCompatError::InvalidPackage(format!(
                "PCA2023 release lock must contain exactly one {expected_name} entry"
            )));
        }
        let package = matches[0];
        if package.target_wim_architecture != expected_architecture {
            return Err(PcaCompatError::InvalidPackage(format!(
                "PCA2023 release lock architecture mismatch for {expected_name}"
            )));
        }
        if package.size == 0 || package.size > MAX_PACKAGE_BYTES {
            return Err(PcaCompatError::InvalidPackage(format!(
                "PCA2023 release lock size is invalid for {expected_name}"
            )));
        }
        for (label, digest) in [
            ("package", package.sha256.as_str()),
            ("bootmgfw_EX.efi", package.bootmgfw_sha256.as_str()),
        ] {
            let digest = normalize_hash(digest);
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(PcaCompatError::InvalidPackage(format!(
                    "PCA2023 release lock {label} SHA-256 is invalid for {expected_name}"
                )));
            }
        }
    }

    resource_lock
        .packages
        .into_iter()
        .find(|package| package.file == file_name)
        .ok_or_else(|| {
            PcaCompatError::InvalidPackage(format!(
                "PCA2023 release lock has no entry for {file_name}"
            ))
        })
}

fn validate_locked_package(
    path: &Path,
    image_index: u32,
    expected_architecture: u16,
    locked: &LockedPcaPackage,
) -> Result<(), PcaCompatError> {
    validate_local_package_file(path)?;
    if image_index != PACKAGE_IMAGE_INDEX {
        return Err(PcaCompatError::InvalidPackage(format!(
            "locked PCA2023 resource package must use image index {PACKAGE_IMAGE_INDEX}, got {image_index}"
        )));
    }
    if expected_architecture != locked.target_wim_architecture {
        return Err(PcaCompatError::InvalidPackage(format!(
            "locked PCA2023 resource architecture mismatch: expected {expected_architecture}, lock has {}",
            locked.target_wim_architecture
        )));
    }
    let size = fs::symlink_metadata(path)
        .map_err(|error| PcaCompatError::Io(error.to_string()))?
        .len();
    if size != locked.size {
        return Err(PcaCompatError::PackageIntegrity(format!(
            "locked package size mismatch: expected {}, actual {size}",
            locked.size
        )));
    }
    verify_sha256_file(path, &locked.sha256)?;

    let temp = ScopedTempDir::create_in(&std::env::temp_dir(), "LetRecovery-PcaCompat")
        .map_err(|error| PcaCompatError::Io(error.to_string()))?;
    {
        let manager = WimlibManager::new().map_err(PcaCompatError::InvalidPackage)?;
        let package = path.to_string_lossy();
        for required in [BOOTEX_BOOT_MANAGER, BOOTEX_FONTS] {
            if !manager
                .image_contains_path(&package, image_index, required)
                .map_err(PcaCompatError::InvalidPackage)?
            {
                return Err(PcaCompatError::InvalidPackage(format!(
                    "resource package is missing allowlisted path: {required}"
                )));
            }
        }
        let target = temp.path().to_string_lossy();
        manager
            .extract_paths(&package, image_index, &target, &[BOOTEX_BOOT_MANAGER])
            .map_err(PcaCompatError::InvalidPackage)?;
    }

    validate_locked_bootex(
        &temp
            .path()
            .join("Windows")
            .join("Boot")
            .join("EFI_EX")
            .join("bootmgfw_EX.efi"),
        expected_architecture,
        locked,
    )
}

fn validate_locked_bootex(
    path: &Path,
    expected_architecture: u16,
    locked: &LockedPcaPackage,
) -> Result<(), PcaCompatError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        PcaCompatError::InvalidPackage(format!(
            "locked bootmgfw_EX.efi is unavailable at {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(PcaCompatError::InvalidPackage(format!(
            "locked bootmgfw_EX.efi is not a regular file: {}",
            path.display()
        )));
    }
    verify_sha256_file(path, &locked.bootmgfw_sha256)?;
    let architecture = inspect_efi_architecture(path);
    if architecture != Some(expected_architecture) {
        return Err(PcaCompatError::InvalidPackage(format!(
            "locked BootEx architecture mismatch: expected {expected_architecture}, actual {architecture:?}"
        )));
    }
    let (generation, issuer) =
        inspect_efi_embedded_signer(path).map_err(PcaCompatError::InvalidPackage)?;
    if generation != PcaGeneration::Pca2023 {
        return Err(PcaCompatError::InvalidPackage(format!(
            "locked bootmgfw_EX.efi signer is not Windows UEFI CA 2023: {issuer}"
        )));
    }
    Ok(())
}

fn validate_local_package_file(path: &Path) -> Result<(), PcaCompatError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| PcaCompatError::MissingOfflineAsset(path.to_path_buf()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(PcaCompatError::InvalidPackage(format!(
            "资源包不是普通文件: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_PACKAGE_BYTES {
        return Err(PcaCompatError::PackageTooLarge(metadata.len()));
    }
    Ok(())
}

fn validate_package_wim(
    path: &Path,
    image_index: u32,
    expected_architecture: u16,
) -> Result<(), PcaCompatError> {
    if image_index == 0 || image_index > 100 {
        return Err(PcaCompatError::InvalidPackage(format!(
            "无效的资源包卷索引: {image_index}"
        )));
    }
    let manager = WimlibManager::new().map_err(PcaCompatError::InvalidPackage)?;
    let package = path.to_string_lossy();
    for required in [BOOTEX_BOOT_MANAGER, BOOTEX_FONTS] {
        if !manager
            .image_contains_path(&package, image_index, required)
            .map_err(PcaCompatError::InvalidPackage)?
        {
            return Err(PcaCompatError::InvalidPackage(format!(
                "资源包缺少白名单路径: {required}"
            )));
        }
    }
    let details = inspect_wim_boot_source_details(path, image_index)
        .map_err(PcaCompatError::InvalidPackage)?;
    if !details.sources.supports(PcaGeneration::Pca2023) {
        return Err(PcaCompatError::InvalidPackage(
            "资源包没有有效 PCA2023 签名的 bootmgfw_EX.efi".to_string(),
        ));
    }
    if details.bootex_architecture != Some(expected_architecture) {
        return Err(PcaCompatError::InvalidPackage(format!(
            "BootEx 架构不匹配：期望 {expected_architecture}，实际 {:?}",
            details.bootex_architecture
        )));
    }
    Ok(())
}

fn verify_sha256_file(path: &Path, expected: &str) -> Result<(), PcaCompatError> {
    let normalized = normalize_hash(expected);
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PcaCompatError::PackageIntegrity(
            "SHA-256 格式无效".to_string(),
        ));
    }
    let actual =
        sha256_file(path, |_| {}).map_err(|error| PcaCompatError::Io(error.to_string()))?;
    if !hash_matches(&actual, &normalized) {
        return Err(PcaCompatError::PackageIntegrity(format!(
            "期望 {normalized}，实际 {actual}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_stable_offline_families_for_supported_targets() {
        assert_eq!(
            select_offline_asset(TargetImageIdentity {
                build: 19_045,
                architecture: 9,
            })
            .unwrap()
            .file_name,
            LEGACY_AMD64_PACKAGE
        );
        assert_eq!(
            select_offline_asset(TargetImageIdentity {
                build: 26_100,
                architecture: 9,
            })
            .unwrap()
            .file_name,
            MODERN_AMD64_PACKAGE
        );
        assert_eq!(
            select_offline_asset(TargetImageIdentity {
                build: 14_393,
                architecture: 0,
            })
            .unwrap()
            .file_name,
            LEGACY_X86_PACKAGE
        );
    }

    #[test]
    fn rejects_arm64_unknown_and_impossible_x86_windows11_targets() {
        for target in [
            TargetImageIdentity {
                build: 26_100,
                architecture: 12,
            },
            TargetImageIdentity {
                build: 26_100,
                architecture: 0,
            },
            TargetImageIdentity {
                build: 19_045,
                architecture: 5,
            },
        ] {
            assert!(matches!(
                select_offline_asset(target),
                Err(PcaCompatError::UnsupportedTarget(_))
            ));
        }
    }

    #[test]
    fn staged_paths_are_confined_to_the_data_directory() {
        let root = Path::new("X:\\LetRecovery");
        assert_eq!(
            resolve_staged_package_path(root, STAGED_PACKAGE_RELATIVE_PATH).unwrap(),
            root.join(STAGED_PACKAGE_RELATIVE_PATH)
        );
        for invalid in [
            "",
            "..\\package.wim",
            "C:\\package.wim",
            "pca_compat/package.zip",
        ] {
            assert!(resolve_staged_package_path(root, invalid).is_err());
        }
    }

    #[test]
    fn invalid_hash_is_distinct_from_hash_mismatch() {
        let path = std::env::temp_dir().join("letrecovery-pca-hash-test.bin");
        fs::write(&path, b"pca").unwrap();
        let invalid = verify_sha256_file(&path, "xyz").unwrap_err();
        assert!(invalid.to_string().contains("格式无效"));
        let mismatch = verify_sha256_file(&path, &"0".repeat(64)).unwrap_err();
        assert!(mismatch.to_string().contains("期望"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn embedded_release_lock_is_complete_and_matches_selection() {
        for (file_name, architecture) in [
            (LEGACY_AMD64_PACKAGE, 9),
            (LEGACY_X86_PACKAGE, 0),
            (MODERN_AMD64_PACKAGE, 9),
        ] {
            let package = locked_package(file_name).unwrap();
            assert_eq!(package.file, file_name);
            assert_eq!(package.target_wim_architecture, architecture);
            assert!(package.size > 0);
            assert_eq!(normalize_hash(&package.sha256).len(), 64);
            assert_eq!(normalize_hash(&package.bootmgfw_sha256).len(), 64);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "validates the three release WIMs from pkg/bin/pca2023"]
    fn validates_all_locked_release_packages_without_host_root_trust() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("pkg")
            .join("bin")
            .join("pca2023");
        for (file_name, architecture) in [
            (LEGACY_AMD64_PACKAGE, 9),
            (LEGACY_X86_PACKAGE, 0),
            (MODERN_AMD64_PACKAGE, 9),
        ] {
            let locked = locked_package(file_name).unwrap();
            validate_locked_package(
                &directory.join(file_name),
                PACKAGE_IMAGE_INDEX,
                architecture,
                &locked,
            )
            .unwrap();
        }
    }
}
