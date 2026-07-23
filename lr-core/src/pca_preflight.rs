//! Pre-format compatibility checks for PCA2011/PCA2023 UEFI boot files.
//!
//! The checks in this module are deliberately read-only. They inspect the
//! selected WIM image before DiskPart or formatting can alter the target disk.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::boot_pca::{
    inspect_efi_architecture, inspect_windows_boot_sources, BootPcaMode, FirmwarePcaInfo,
    PcaGeneration, WindowsBootSources,
};
use crate::wimlib::WimlibManager;

const NORMAL_BOOT_MANAGER: &str = "\\Windows\\Boot\\EFI\\bootmgfw.efi";
const BOOTEX_BOOT_MANAGER: &str = "\\Windows\\Boot\\EFI_EX\\bootmgfw_EX.efi";
const MAX_TEMP_DIR_ATTEMPTS: u64 = 128;
static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallImageKind {
    WimFamily,
    Opaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcaPreflightStatus {
    NotRequired,
    Verified(PcaGeneration),
    CompatibilityPackageRequired(PcaGeneration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PcaPreflightError {
    Pca2011Revoked,
    Pca2011NotTrusted,
    Pca2023NotTrusted,
    Pca2023TrustUnknown,
    Pca2023UnsupportedForLegacyWindows,
    OpaqueImage(PcaGeneration),
    InspectImage(String),
    UnsupportedArchitecture(u16),
    MissingSource(PcaGeneration),
}

impl fmt::Display for PcaPreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pca2011Revoked => f.write_str("固件已撤销 PCA2011"),
            Self::Pca2011NotTrusted => f.write_str("固件不信任 PCA2011"),
            Self::Pca2023NotTrusted => f.write_str("固件不信任 Windows UEFI CA 2023"),
            Self::Pca2023TrustUnknown => {
                f.write_str("固件无法使用 PCA2011，但无法确认其信任 Windows UEFI CA 2023")
            }
            Self::Pca2023UnsupportedForLegacyWindows => {
                f.write_str("Windows 9x/NT5/Vista/7/8/8.1 不支持 PCA2023 BootEx 升级")
            }
            Self::OpaqueImage(generation) => {
                write!(f, "无法在写盘前验证镜像中的 {generation} EFI 引导文件")
            }
            Self::InspectImage(error) => write!(f, "读取镜像 EFI 引导文件失败: {error}"),
            Self::UnsupportedArchitecture(architecture) => {
                write!(
                    f,
                    "LetRecovery 不支持安装 architecture {architecture} 的系统镜像"
                )
            }
            Self::MissingSource(generation) => {
                write!(f, "镜像缺少有效的 {generation} EFI 引导文件")
            }
        }
    }
}

/// Determine whether a signing generation must be proven before disk writes.
///
/// Auto mode only becomes mandatory when Secure Boot is enabled and PCA2011
/// has been revoked. Explicit user choices are always verified against both
/// firmware policy and image contents.
pub fn required_generation(
    requested: BootPcaMode,
    firmware: &FirmwarePcaInfo,
) -> Result<Option<PcaGeneration>, PcaPreflightError> {
    let secure_boot = firmware.secure_boot_enabled == Some(true);

    match requested {
        BootPcaMode::Pca2011 => {
            if secure_boot && firmware.revokes_pca2011 == Some(true) {
                return Err(PcaPreflightError::Pca2011Revoked);
            }
            if secure_boot && firmware.trusts_pca2011 == Some(false) {
                return Err(PcaPreflightError::Pca2011NotTrusted);
            }
            Ok(Some(PcaGeneration::Pca2011))
        }
        BootPcaMode::Pca2023 => {
            if secure_boot && firmware.trusts_pca2023 == Some(false) {
                return Err(PcaPreflightError::Pca2023NotTrusted);
            }
            Ok(Some(PcaGeneration::Pca2023))
        }
        BootPcaMode::Auto => {
            let pca2011_unusable =
                firmware.revokes_pca2011 == Some(true) || firmware.trusts_pca2011 == Some(false);
            if !secure_boot || !pca2011_unusable {
                return Ok(None);
            }
            match firmware.trusts_pca2023 {
                Some(true) => Ok(Some(PcaGeneration::Pca2023)),
                Some(false) => Err(PcaPreflightError::Pca2023NotTrusted),
                None => Err(PcaPreflightError::Pca2023TrustUnknown),
            }
        }
    }
}

pub fn assess_sources(
    requested: BootPcaMode,
    firmware: &FirmwarePcaInfo,
    sources: &WindowsBootSources,
) -> Result<PcaPreflightStatus, PcaPreflightError> {
    let required = required_generation(requested, firmware)?;
    let preferred = required.or_else(|| {
        (requested == BootPcaMode::Auto && firmware.trusts_pca2023 == Some(true))
            .then_some(PcaGeneration::Pca2023)
    });
    let Some(required) = preferred else {
        return Ok(PcaPreflightStatus::NotRequired);
    };

    if sources.supports(required) {
        Ok(PcaPreflightStatus::Verified(required))
    } else if required == PcaGeneration::Pca2023 {
        Ok(PcaPreflightStatus::CompatibilityPackageRequired(required))
    } else {
        Err(PcaPreflightError::MissingSource(required))
    }
}

/// Verify the selected install image before any target-disk mutation.
pub fn verify_install_image(
    image_file: &Path,
    index: u32,
    image_kind: InstallImageKind,
    may_use_uefi: bool,
    is_nt5: bool,
    requested: BootPcaMode,
    firmware: &FirmwarePcaInfo,
) -> Result<PcaPreflightStatus, PcaPreflightError> {
    if is_nt5 {
        return Ok(PcaPreflightStatus::NotRequired);
    }

    let mut selected_major = None;
    if image_kind == InstallImageKind::WimFamily {
        let (major, architecture) = selected_image_platform(image_file, index)?;
        if !matches!(architecture, 0 | 9) {
            return Err(PcaPreflightError::UnsupportedArchitecture(architecture));
        }
        selected_major = Some(major);
    }

    if !may_use_uefi {
        return Ok(PcaPreflightStatus::NotRequired);
    }
    if selected_major.is_some_and(|major| major != 10) {
        verify_legacy_windows_firmware(requested, firmware)?;
        return Ok(PcaPreflightStatus::NotRequired);
    }

    let required = required_generation(requested, firmware)?;
    let preferred = required.or_else(|| {
        (requested == BootPcaMode::Auto && firmware.trusts_pca2023 == Some(true))
            .then_some(PcaGeneration::Pca2023)
    });
    let Some(required) = preferred else {
        return Ok(PcaPreflightStatus::NotRequired);
    };

    if image_kind == InstallImageKind::Opaque {
        return Err(PcaPreflightError::OpaqueImage(required));
    }

    let sources =
        inspect_wim_boot_sources(image_file, index).map_err(PcaPreflightError::InspectImage)?;
    if sources.supports(required) {
        Ok(PcaPreflightStatus::Verified(required))
    } else if required == PcaGeneration::Pca2023 {
        Ok(PcaPreflightStatus::CompatibilityPackageRequired(required))
    } else {
        Err(PcaPreflightError::MissingSource(required))
    }
}

fn verify_legacy_windows_firmware(
    requested: BootPcaMode,
    firmware: &FirmwarePcaInfo,
) -> Result<(), PcaPreflightError> {
    if requested == BootPcaMode::Pca2023 {
        return Err(PcaPreflightError::Pca2023UnsupportedForLegacyWindows);
    }
    if firmware.secure_boot_enabled == Some(true) {
        if firmware.revokes_pca2011 == Some(true) {
            return Err(PcaPreflightError::Pca2011Revoked);
        }
        if firmware.trusts_pca2011 == Some(false) {
            return Err(PcaPreflightError::Pca2011NotTrusted);
        }
    }
    Ok(())
}

/// PCA generation selection is exposed only for Windows 10/11 and Server
/// 2016+, on architectures supported by LetRecovery's bundled toolchain.
pub const fn supports_pca_selection(major: Option<u16>, architecture: Option<u16>) -> bool {
    matches!(major, Some(10)) && matches!(architecture, Some(0 | 9))
}

fn selected_image_platform(image_file: &Path, index: u32) -> Result<(u16, u16), PcaPreflightError> {
    let image = image_file
        .to_str()
        .ok_or_else(|| PcaPreflightError::InspectImage("镜像路径不是有效 Unicode".to_string()))?;
    let images = WimlibManager::new()
        .map_err(PcaPreflightError::InspectImage)?
        .get_image_info(image)
        .map_err(PcaPreflightError::InspectImage)?;
    let selected = images
        .iter()
        .find(|candidate| candidate.index == index)
        .ok_or_else(|| PcaPreflightError::InspectImage(format!("镜像卷索引 {index} 不存在")))?;
    let major = selected
        .major_version
        .ok_or_else(|| PcaPreflightError::InspectImage("WIM XML 缺少 VERSION/MAJOR".to_string()))?;
    let architecture = selected
        .architecture
        .ok_or_else(|| PcaPreflightError::InspectImage("WIM XML 缺少 WINDOWS/ARCH".to_string()))?;
    Ok((major, architecture))
}

/// Extract only the two possible Windows boot managers and validate their
/// embedded signatures. No image is mounted and no target volume is touched.
pub fn inspect_wim_boot_sources(
    image_file: &Path,
    index: u32,
) -> Result<WindowsBootSources, String> {
    inspect_wim_boot_source_details(image_file, index).map(|details| details.sources)
}

#[derive(Debug, Clone, Default)]
pub struct WimBootSourceDetails {
    pub sources: WindowsBootSources,
    pub bootex_architecture: Option<u16>,
}

pub fn inspect_wim_boot_source_details(
    image_file: &Path,
    index: u32,
) -> Result<WimBootSourceDetails, String> {
    let image = image_file
        .to_str()
        .ok_or_else(|| format!("镜像路径不是有效 Unicode: {}", image_file.display()))?;
    let manager = WimlibManager::new()?;
    let mut paths = Vec::with_capacity(2);

    for path in [NORMAL_BOOT_MANAGER, BOOTEX_BOOT_MANAGER] {
        if manager.image_contains_path(image, index, path)? {
            paths.push(path);
        }
    }

    if paths.is_empty() {
        return Ok(WimBootSourceDetails::default());
    }

    let temp_dir = ScopedTempDir::create()?;
    let target = temp_dir.path().to_string_lossy();
    manager.extract_paths(image, index, &target, &paths)?;

    let windows = temp_dir.path().join("Windows");
    Ok(WimBootSourceDetails {
        sources: inspect_windows_boot_sources(&windows),
        bootex_architecture: inspect_efi_architecture(
            &windows.join("Boot").join("EFI_EX").join("bootmgfw_EX.efi"),
        ),
    })
}

#[derive(Debug)]
struct ScopedTempDir {
    path: PathBuf,
}

impl ScopedTempDir {
    fn create() -> Result<Self, String> {
        let base = std::env::temp_dir();
        Self::create_in(&base)
    }

    fn create_in(base: &Path) -> Result<Self, String> {
        fs::create_dir_all(base).map_err(|error| {
            format!("创建 PCA 预检临时根目录失败 ({}): {error}", base.display())
        })?;
        let metadata = fs::symlink_metadata(base).map_err(|error| {
            format!("检查 PCA 预检临时根目录失败 ({}): {error}", base.display())
        })?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "PCA 预检临时根路径不是安全的普通目录: {}",
                base.display()
            ));
        }

        for _ in 0..MAX_TEMP_DIR_ATTEMPTS {
            let id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "LetRecovery-PcaPreflight-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("创建 PCA 预检临时目录失败: {error}")),
            }
        }
        Err("无法分配唯一的 PCA 预检临时目录".to_string())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            log::warn!(
                "[BOOT PCA] 清理预检临时目录 {} 失败: {}",
                self.path.display(),
                error
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot_pca::EfiSignatureInfo;

    fn firmware() -> FirmwarePcaInfo {
        FirmwarePcaInfo {
            secure_boot_enabled: Some(true),
            trusts_pca2011: Some(true),
            trusts_pca2023: Some(true),
            revokes_pca2011: Some(false),
            error: None,
        }
    }

    fn sources(generation: PcaGeneration) -> WindowsBootSources {
        let info = EfiSignatureInfo {
            generation,
            signature_valid: true,
            ..Default::default()
        };
        match generation {
            PcaGeneration::Pca2011 => WindowsBootSources {
                pca2011: Some(info),
                pca2023: None,
            },
            PcaGeneration::Pca2023 => WindowsBootSources {
                pca2011: None,
                pca2023: Some(info),
            },
            PcaGeneration::Unknown => WindowsBootSources::default(),
        }
    }

    #[test]
    fn auto_without_revocation_does_not_reject_old_images() {
        assert_eq!(
            required_generation(BootPcaMode::Auto, &firmware()).unwrap(),
            None
        );
    }

    #[test]
    fn auto_prepares_bootex_when_firmware_explicitly_trusts_pca2023() {
        assert_eq!(
            assess_sources(
                BootPcaMode::Auto,
                &firmware(),
                &sources(PcaGeneration::Pca2011)
            ),
            Ok(PcaPreflightStatus::CompatibilityPackageRequired(
                PcaGeneration::Pca2023
            ))
        );
        assert_eq!(
            assess_sources(
                BootPcaMode::Auto,
                &firmware(),
                &sources(PcaGeneration::Pca2023)
            ),
            Ok(PcaPreflightStatus::Verified(PcaGeneration::Pca2023))
        );
    }

    #[test]
    fn auto_does_not_upgrade_when_pca2023_trust_is_absent() {
        let mut fw = firmware();
        fw.trusts_pca2023 = Some(false);
        assert_eq!(
            assess_sources(BootPcaMode::Auto, &fw, &sources(PcaGeneration::Pca2011)),
            Ok(PcaPreflightStatus::NotRequired)
        );
    }

    #[test]
    fn revoked_pca2011_requires_verified_pca2023_source() {
        let mut fw = firmware();
        fw.revokes_pca2011 = Some(true);

        assert_eq!(
            assess_sources(BootPcaMode::Auto, &fw, &sources(PcaGeneration::Pca2023)),
            Ok(PcaPreflightStatus::Verified(PcaGeneration::Pca2023))
        );
        assert_eq!(
            assess_sources(BootPcaMode::Auto, &fw, &sources(PcaGeneration::Pca2011)),
            Ok(PcaPreflightStatus::CompatibilityPackageRequired(
                PcaGeneration::Pca2023
            ))
        );
    }

    #[test]
    fn firmware_that_no_longer_trusts_pca2011_requires_pca2023() {
        let mut fw = firmware();
        fw.trusts_pca2011 = Some(false);

        assert_eq!(
            required_generation(BootPcaMode::Auto, &fw),
            Ok(Some(PcaGeneration::Pca2023))
        );
        assert_eq!(
            assess_sources(BootPcaMode::Auto, &fw, &sources(PcaGeneration::Pca2011)),
            Ok(PcaPreflightStatus::CompatibilityPackageRequired(
                PcaGeneration::Pca2023
            ))
        );
    }

    #[test]
    fn explicit_generation_is_checked_even_without_revocation() {
        assert_eq!(
            assess_sources(
                BootPcaMode::Pca2023,
                &firmware(),
                &sources(PcaGeneration::Pca2011)
            ),
            Ok(PcaPreflightStatus::CompatibilityPackageRequired(
                PcaGeneration::Pca2023
            ))
        );
    }

    #[test]
    fn legacy_still_validates_architecture_while_nt5_skips_modern_preflight() {
        let missing = std::env::temp_dir().join("definitely-missing-letrecovery.wim");
        assert!(matches!(
            verify_install_image(
                &missing,
                1,
                InstallImageKind::WimFamily,
                false,
                false,
                BootPcaMode::Pca2023,
                &firmware(),
            ),
            Err(PcaPreflightError::InspectImage(_))
        ));
        assert_eq!(
            verify_install_image(
                &missing,
                1,
                InstallImageKind::WimFamily,
                true,
                true,
                BootPcaMode::Pca2023,
                &firmware(),
            ),
            Ok(PcaPreflightStatus::NotRequired)
        );
    }

    #[test]
    fn pca_controls_apply_only_to_supported_modern_windows_images() {
        assert!(supports_pca_selection(Some(10), Some(9)));
        assert!(supports_pca_selection(Some(10), Some(0)));
        assert!(!supports_pca_selection(Some(6), Some(9)));
        assert!(!supports_pca_selection(Some(10), Some(12)));
        assert!(!supports_pca_selection(None, Some(9)));
    }

    #[test]
    fn legacy_windows_is_allowed_only_while_pca2011_remains_bootable() {
        assert!(verify_legacy_windows_firmware(BootPcaMode::Auto, &firmware()).is_ok());

        let mut revoked = firmware();
        revoked.revokes_pca2011 = Some(true);
        assert_eq!(
            verify_legacy_windows_firmware(BootPcaMode::Auto, &revoked),
            Err(PcaPreflightError::Pca2011Revoked)
        );

        let mut secure_boot_off = revoked;
        secure_boot_off.secure_boot_enabled = Some(false);
        assert!(verify_legacy_windows_firmware(BootPcaMode::Auto, &secure_boot_off).is_ok());
        assert_eq!(
            verify_legacy_windows_firmware(BootPcaMode::Pca2023, &firmware()),
            Err(PcaPreflightError::Pca2023UnsupportedForLegacyWindows)
        );
    }

    #[test]
    fn opaque_image_fails_closed_when_generation_is_required() {
        assert_eq!(
            verify_install_image(
                Path::new("opaque.gho"),
                1,
                InstallImageKind::Opaque,
                true,
                false,
                BootPcaMode::Pca2023,
                &firmware(),
            ),
            Err(PcaPreflightError::OpaqueImage(PcaGeneration::Pca2023))
        );
    }

    #[test]
    fn auto_revocation_requires_confirmed_pca2023_trust() {
        let mut fw = firmware();
        fw.revokes_pca2011 = Some(true);
        fw.trusts_pca2023 = None;
        assert_eq!(
            required_generation(BootPcaMode::Auto, &fw),
            Err(PcaPreflightError::Pca2023TrustUnknown)
        );
    }

    #[test]
    fn preflight_temp_directory_creates_a_missing_parent_tree() {
        let root = std::env::temp_dir().join(format!(
            "letrecovery-pca-temp-parent-{}-{}",
            std::process::id(),
            NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let base = root.join("missing").join("temp");
        assert!(!base.exists());

        let directory = ScopedTempDir::create_in(&base).unwrap();
        assert!(base.is_dir());
        assert!(directory.path().is_dir());
        assert_eq!(directory.path().parent(), Some(base.as_path()));

        drop(directory);
        fs::remove_dir_all(root).unwrap();
    }
}
