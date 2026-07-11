//! WinPE adapter for the shared read-only PCA install preflight.

use std::path::Path;

use lr_core::boot_pca::{BootPcaMode, PcaGeneration};
use lr_core::pca_compat::PreparedPcaCompatPackage;
use lr_core::pca_preflight::{InstallImageKind, PcaPreflightError, PcaPreflightStatus};

use crate::tr;

use super::config::InstallConfig;

#[derive(Debug)]
pub struct StagedPcaCompatConfig {
    path: std::path::PathBuf,
    sha256: String,
    image_index: u32,
    target: lr_core::pca_compat::TargetImageIdentity,
}

pub fn staged_config(
    config: &InstallConfig,
    data_directory: &Path,
) -> Result<Option<StagedPcaCompatConfig>, String> {
    if config.pca_compat_package.is_empty() {
        let has_partial_metadata = !config.pca_compat_sha256.is_empty()
            || config.pca_compat_image_index != 0
            || config.pca_compat_target_build != 0
            || config.pca_compat_target_architecture != 0;
        return if has_partial_metadata {
            Err(tr!("PCA2023 兼容包配置不完整，安装已在格式化前停止"))
        } else {
            Ok(None)
        };
    }
    if config.pca_compat_sha256.is_empty()
        || config.pca_compat_image_index == 0
        || config.pca_compat_target_build == 0
        || !matches!(config.pca_compat_target_architecture, 0 | 9)
    {
        return Err(tr!("PCA2023 兼容包配置不完整，安装已在格式化前停止"));
    }
    let path = lr_core::pca_compat::resolve_staged_package_path(
        data_directory,
        &config.pca_compat_package,
    )
    .map_err(|error| tr!("PCA2023 兼容包配置无效：{}", error))?;
    Ok(Some(StagedPcaCompatConfig {
        path,
        sha256: config.pca_compat_sha256.clone(),
        image_index: config.pca_compat_image_index,
        target: lr_core::pca_compat::TargetImageIdentity {
            build: config.pca_compat_target_build,
            architecture: config.pca_compat_target_architecture,
        },
    }))
}

pub fn verify_before_disk_write(
    image_path: &str,
    volume_index: u32,
    is_gho: bool,
    is_nt5: bool,
    may_use_uefi: bool,
    requested: BootPcaMode,
    staged: Option<&StagedPcaCompatConfig>,
) -> Result<Option<PreparedPcaCompatPackage>, String> {
    let firmware = lr_core::boot_pca::inspect_firmware_pca();
    if let Some(error) = firmware.error.as_deref() {
        log::warn!("[BOOT PCA] 固件预检信息不完整: {error}");
    }

    let image_kind = if is_gho {
        InstallImageKind::Opaque
    } else {
        InstallImageKind::WimFamily
    };
    match lr_core::pca_preflight::verify_install_image(
        Path::new(image_path),
        volume_index,
        image_kind,
        may_use_uefi,
        is_nt5,
        requested,
        &firmware,
    ) {
        Ok(PcaPreflightStatus::NotRequired) => {
            log::info!("[BOOT PCA] 写盘前预检无需强制签名代际");
            Ok(None)
        }
        Ok(PcaPreflightStatus::Verified(generation)) => {
            log::info!("[BOOT PCA] 写盘前预检通过: {generation}");
            Ok(None)
        }
        Ok(PcaPreflightStatus::CompatibilityPackageRequired(generation)) => {
            log::info!("[BOOT PCA] 镜像缺少 {generation}，准备离线资源包");
            if let Some(staged) = staged {
                return lr_core::pca_compat::open_staged_package(
                    Path::new(image_path),
                    volume_index,
                    &staged.path,
                    &staged.sha256,
                    staged.image_index,
                    staged.target,
                )
                .map(Some)
                .map_err(|error| {
                    log::error!("[BOOT PCA] 暂存兼容包验证失败: {error}");
                    tr!(
                        "暂存的 PCA2023 兼容包校验失败：{}。安装已在格式化前停止。",
                        error
                    )
                });
            }
            lr_core::pca_compat::prepare_from_local_assets(
                Path::new(image_path),
                volume_index,
                &crate::utils::path::get_bin_dir().join("pca2023"),
            )
            .map(Some)
            .map_err(|error| {
                log::error!("[BOOT PCA] 自动准备兼容包失败: {error}");
                tr!(
                    "准备适用于所选系统镜像的 PCA2023 离线资源失败：{}。安装已在格式化前停止。",
                    error
                )
            })
        }
        Err(error) => {
            log::error!("[BOOT PCA] 写盘前预检失败: {error}");
            Err(user_error(&error))
        }
    }
}

fn user_error(error: &PcaPreflightError) -> String {
    let detail = match error {
        PcaPreflightError::Pca2011Revoked => {
            tr!("固件已撤销 PCA2011，不能写入 PCA2011 引导")
        }
        PcaPreflightError::Pca2011NotTrusted => {
            tr!("固件不信任 PCA2011，不能写入 PCA2011 引导")
        }
        PcaPreflightError::Pca2023NotTrusted => {
            tr!("固件不信任 Windows UEFI CA 2023，不能写入 PCA2023 引导")
        }
        PcaPreflightError::Pca2023TrustUnknown => tr!(
            "固件无法使用 PCA2011，但无法确认其信任 Windows UEFI CA 2023"
        ),
        PcaPreflightError::Pca2023UnsupportedForLegacyWindows => tr!(
            "所选旧版 Windows 不支持 PCA2023 BootEx；请关闭 Secure Boot 或改用 Windows 10/11"
        ),
        PcaPreflightError::OpaqueImage(PcaGeneration::Pca2011) => tr!(
            "当前安装要求 PCA2011，但 GHO 镜像无法在写盘前验证 EFI 引导签名；请改用 WIM/ESD 镜像"
        ),
        PcaPreflightError::OpaqueImage(_) => tr!(
            "当前安装要求 PCA2023，但 GHO 镜像无法在写盘前验证 EFI 引导签名；请改用已集成相应微软更新的 WIM/ESD 镜像"
        ),
        PcaPreflightError::InspectImage(_) => tr!(
            "无法在写盘前验证系统镜像的 EFI 引导文件；请检查镜像和 libwim 后重试"
        ),
        PcaPreflightError::UnsupportedArchitecture(_) => tr!(
            "所选系统镜像的架构不受支持；LetRecovery 仅支持 x64 和 x86 系统镜像"
        ),
        PcaPreflightError::MissingSource(PcaGeneration::Pca2011) => {
            tr!("所选系统镜像不包含有效的 PCA2011 EFI 引导文件")
        }
        PcaPreflightError::MissingSource(_) => tr!(
            "所选系统镜像不包含有效的 PCA2023 BootEx 引导文件；请换用已集成相应微软更新的镜像"
        ),
    };
    tr!(
        "PCA 引导兼容性检查失败：{}。为避免安装后无法启动，已在格式化前停止。",
        detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_configs_do_not_require_a_staged_package() {
        assert!(
            staged_config(&InstallConfig::default(), Path::new("X:\\data"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn partial_or_traversing_staged_config_is_rejected() {
        let partial = InstallConfig {
            pca_compat_sha256: "a".repeat(64),
            ..InstallConfig::default()
        };
        assert!(staged_config(&partial, Path::new("X:\\data")).is_err());

        let traversing = InstallConfig {
            pca_compat_package: "..\\package.wim".to_string(),
            pca_compat_sha256: "a".repeat(64),
            pca_compat_image_index: 1,
            pca_compat_target_build: 19045,
            pca_compat_target_architecture: 9,
            ..InstallConfig::default()
        };
        assert!(staged_config(&traversing, Path::new("X:\\data")).is_err());
    }
}
