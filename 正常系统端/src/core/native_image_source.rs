//! Image-source inspection for the native install page.
//!
//! ISO attachment is read-only but still changes host mount state, so the
//! non-elevated development feature refuses it. WIM-family metadata reads and
//! GHO classification do not mutate the source or any target disk.

use std::path::{Path, PathBuf};

use super::dism::ImageInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageSourceKind {
    WimFamily,
    Ghost,
    Iso,
    Unsupported,
}

#[derive(Clone, Debug)]
pub enum InspectedImageSource {
    WimFamily {
        selected_path: PathBuf,
        effective_image_path: PathBuf,
        volumes: Vec<ImageInfo>,
        mounted_iso: Option<PathBuf>,
    },
    Ghost {
        path: PathBuf,
    },
    XpTextMode {
        selected_path: PathBuf,
        i386_directory: PathBuf,
        mounted_iso: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImageSourceError {
    #[error("请选择存在的普通系统镜像文件")]
    MissingOrInvalidFile,
    #[error("不支持此系统镜像格式")]
    UnsupportedFormat,
    #[error("开发测试构建禁止挂载 ISO")]
    IsoMountDisabledInDevelopment,
    #[error("ISO 挂载失败: {0}")]
    IsoMount(String),
    #[error("ISO 中未找到 install.wim、install.esd、install.swm 或完整 XP/2003 安装源")]
    IsoHasNoInstallSource,
    #[error("读取系统镜像卷失败: {0}")]
    Metadata(String),
}

pub fn classify_image_source(path: &Path) -> ImageSourceKind {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("wim" | "esd" | "swm") => ImageSourceKind::WimFamily,
        Some("gho" | "ghs") => ImageSourceKind::Ghost,
        Some("iso") => ImageSourceKind::Iso,
        _ => ImageSourceKind::Unsupported,
    }
}

pub fn inspect_image_source(
    path: impl AsRef<Path>,
) -> Result<InspectedImageSource, ImageSourceError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(ImageSourceError::MissingOrInvalidFile);
    }
    match classify_image_source(path) {
        ImageSourceKind::Ghost => Ok(InspectedImageSource::Ghost {
            path: path.to_path_buf(),
        }),
        ImageSourceKind::WimFamily => inspect_wim(path, path, None),
        ImageSourceKind::Iso => inspect_iso(path),
        ImageSourceKind::Unsupported => Err(ImageSourceError::UnsupportedFormat),
    }
}

fn inspect_wim(
    selected_path: &Path,
    effective_path: &Path,
    mounted_iso: Option<PathBuf>,
) -> Result<InspectedImageSource, ImageSourceError> {
    let volumes = super::dism::Dism::new()
        .get_image_info(&effective_path.to_string_lossy())
        .map_err(|error| ImageSourceError::Metadata(error.to_string()))?;
    Ok(InspectedImageSource::WimFamily {
        selected_path: selected_path.to_path_buf(),
        effective_image_path: effective_path.to_path_buf(),
        volumes,
        mounted_iso,
    })
}

/// Read-only compatibility probe for answer files supplied by the selected media or embedded
/// in the selected WIM-family image. Probe failures are logged and treated as “not detected”,
/// matching the legacy default-selection behavior without blocking image inspection.
pub fn source_has_unattend(image_path: &Path, image_index: u32) -> bool {
    if image_path.as_os_str().is_empty() {
        return false;
    }
    if lr_core::xp_i386::is_valid_i386(image_path) {
        return image_path.join("winnt.sif").is_file()
            || image_path
                .parent()
                .is_some_and(|root| root.join("winnt.sif").is_file());
    }

    let text = image_path.to_string_lossy();
    let lower = text.to_ascii_lowercase();
    let base = lower
        .find("\\sources\\")
        .and_then(|offset| (offset >= 2).then(|| PathBuf::from(format!("{}\\", &text[..2]))));
    let base = base.or_else(|| image_path.parent().map(Path::to_path_buf));
    if base.is_some_and(|base| {
        [
            "autounattend.xml",
            "Autounattend.xml",
            "AutoUnattend.xml",
            "unattend.xml",
            "Unattend.xml",
        ]
        .iter()
        .any(|name| base.join(name).is_file())
    }) {
        return true;
    }

    if !matches!(
        image_path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("wim" | "esd" | "swm")
    ) {
        return false;
    }
    const EMBEDDED_PATHS: [&str; 4] = [
        "\\Windows\\Panther\\unattend.xml",
        "\\Windows\\Panther\\Autounattend.xml",
        "\\Windows\\System32\\Sysprep\\unattend.xml",
        "\\unattend.xml",
    ];
    match lr_core::WimEngineManager::new_current()
        .and_then(|manager| manager.image_contains_any_path(&text, image_index, &EMBEDDED_PATHS))
    {
        Ok(found) => found,
        Err(error) => {
            log::warn!("source answer-file probe failed and was ignored: {error}");
            false
        }
    }
}

#[cfg(feature = "non-elevated-tests")]
fn inspect_iso(_path: &Path) -> Result<InspectedImageSource, ImageSourceError> {
    Err(ImageSourceError::IsoMountDisabledInDevelopment)
}

#[cfg(not(feature = "non-elevated-tests"))]
fn inspect_iso(path: &Path) -> Result<InspectedImageSource, ImageSourceError> {
    let selected = path.to_path_buf();
    let drive = super::iso::IsoMounter::mount_iso(&path.to_string_lossy())
        .map_err(|error| ImageSourceError::IsoMount(error.to_string()))?;
    if let Some(image) = super::iso::IsoMounter::find_install_image_in_drive(&drive) {
        return inspect_wim(&selected, Path::new(&image), Some(selected.clone()));
    }
    if let Some(i386) = super::iso::IsoMounter::xp_i386_dir(&drive) {
        return Ok(InspectedImageSource::XpTextMode {
            selected_path: selected.clone(),
            i386_directory: PathBuf::from(i386),
            mounted_iso: Some(selected),
        });
    }
    let _ = super::iso::IsoMounter::unmount_iso_by_path(&path.to_string_lossy());
    Err(ImageSourceError::IsoHasNoInstallSource)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions_are_case_insensitive() {
        assert_eq!(
            classify_image_source(Path::new("install.WIM")),
            ImageSourceKind::WimFamily
        );
        assert_eq!(
            classify_image_source(Path::new("backup.GHS")),
            ImageSourceKind::Ghost
        );
        assert_eq!(
            classify_image_source(Path::new("windows.Iso")),
            ImageSourceKind::Iso
        );
    }

    #[test]
    fn archives_and_executables_are_not_install_sources() {
        assert_eq!(
            classify_image_source(Path::new("image.zip")),
            ImageSourceKind::Unsupported
        );
        assert_eq!(
            classify_image_source(Path::new("setup.exe")),
            ImageSourceKind::Unsupported
        );
    }
}
