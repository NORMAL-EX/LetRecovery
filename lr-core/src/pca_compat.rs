//! Offline PCA2023 compatibility assets for supported Windows images.
//!
//! LetRecovery ships a small, fixed set of WIM resource packs. Selection is
//! based on the target image architecture and boot-environment family; no
//! network access is required. Every package is signature-checked before use
//! and SHA-256 binds a package staged by the desktop endpoint to WinPE.

use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::boot_pca::{inspect_windows_boot_sources, PcaGeneration};
use crate::hash::{hash_matches, normalize_hash, sha256_file};
use crate::pca_preflight::inspect_wim_boot_source_details;
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
        verify_sha256_file(&self.path, &self.sha256)?;
        validate_package_wim(&self.path, self.image_index, self.target.architecture)?;

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
        let sources = inspect_windows_boot_sources(&windows);
        if !sources.supports(PcaGeneration::Pca2023) {
            return Err(PcaCompatError::InvalidPackage(
                "注入后未检测到有效的 PCA2023 BootEx 引导文件".to_string(),
            ));
        }
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
    validate_offline_asset_package(&package_path, target.architecture)?;
    let sha256 = sha256_file(&package_path, |_| {})
        .map_err(|error| PcaCompatError::Io(error.to_string()))?;

    Ok(PreparedPcaCompatPackage {
        path: package_path,
        sha256,
        image_index: PACKAGE_IMAGE_INDEX,
        target,
        family: selection.family,
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
    verify_sha256_file(package_path, expected_sha256)?;
    validate_package_wim(
        package_path,
        package_image_index,
        actual_target.architecture,
    )?;
    Ok(PreparedPcaCompatPackage {
        path: package_path.to_path_buf(),
        sha256: normalize_hash(expected_sha256),
        image_index: package_image_index,
        target: actual_target,
        family: selection.family,
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
}
