//! Pure decision boundary for the native system-install page.
//!
//! The controller validates a snapshot and returns an intent. It deliberately
//! does not unlock BitLocker, write configuration files, format, install a PE
//! boot entry, apply an image, or restart the machine.

use lr_core::boot_pca::BootPcaMode;

use crate::core::disk::PartitionStyle;
use crate::core::install_config::InstallConfig;
use crate::core::ui_state::{AdvancedOptionsData, BootModeSelection, DriverAction, InstallPrefs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMode {
    Direct,
    ViaPe,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstallTarget {
    pub partition: String,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
    pub style: PartitionStyle,
    pub is_current_system: bool,
    pub has_windows: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectedImageMetadata {
    pub volume_index: u32,
    pub major_version: Option<u16>,
    pub architecture: Option<u16>,
}

/// Complete UI snapshot needed to decide whether the install button may proceed.
#[derive(Clone, Debug)]
pub struct NativeInstallState {
    pub image_path: String,
    /// False while the path is empty, being mounted, or has not been identified.
    pub image_ready: bool,
    pub selected_image: Option<SelectedImageMetadata>,
    /// XP/2003 text-mode media use this directory instead of a WIM-like image.
    pub xp_i386_source: Option<String>,
    pub target: Option<InstallTarget>,
    pub is_pe_environment: bool,
    pub pe_available: bool,
    pub selected_pe: Option<usize>,
    pub custom_unattend_path: String,
    pub custom_unattend_error: Option<String>,
    pub pca_detection_pending: bool,
    pub pca_selection_error: Option<String>,
    pub advanced_options_enabled: bool,
    pub prefs: InstallPrefs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallValidationError {
    MissingImage,
    ImageNotReady,
    MissingImageVolume,
    UnsupportedImageArchitecture,
    MissingTargetPartition,
    UnstableTargetIdentity,
    PeUnavailable,
    MissingPeSelection,
    InvalidCustomUnattend,
    PcaDetectionPending,
    InvalidPcaSelection,
    XpI386RequiresLegacyMbr,
}

impl std::fmt::Display for InstallValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MissingImage => crate::tr!("请选择系统镜像。"),
            Self::ImageNotReady => crate::tr!("系统镜像仍在读取，请稍候。"),
            Self::MissingImageVolume => crate::tr!("请选择要安装的镜像卷。"),
            Self::UnsupportedImageArchitecture => crate::tr!("仅支持 x86 或 x64 系统镜像。"),
            Self::MissingTargetPartition => crate::tr!("请选择安装目标分区。"),
            Self::UnstableTargetIdentity => {
                crate::tr!("无法确认安装目标的磁盘和分区身份，请刷新后重试。")
            }
            Self::PeUnavailable => crate::tr!("安装到当前系统分区需要可用的 PE 环境。"),
            Self::MissingPeSelection => crate::tr!("请选择用于安装的 PE 环境。"),
            Self::InvalidCustomUnattend => crate::tr!("自定义无人值守文件无效。"),
            Self::PcaDetectionPending => crate::tr!("正在检测 PCA 兼容性，请稍候。"),
            Self::InvalidPcaSelection => crate::tr!("所选 PCA 启动签名与系统镜像不兼容。"),
            Self::XpI386RequiresLegacyMbr => {
                crate::tr!("XP 文本模式安装需要 Legacy/MBR 目标。")
            }
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for InstallValidationError {}

/// Runtime options previously assembled by the egui install button.
#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub format_partition: bool,
    pub repair_boot: bool,
    pub unattended_install: bool,
    pub export_drivers: bool,
    pub auto_reboot: bool,
    pub boot_mode: BootModeSelection,
    pub boot_pca_mode: BootPcaMode,
    pub advanced_options: AdvancedOptionsData,
    pub driver_action: DriverAction,
    pub custom_unattend_path: String,
    pub is_xp: bool,
    pub is_xp_i386: bool,
    pub run_diskpart_scripts: bool,
}

#[derive(Clone, Debug)]
pub struct StartInstallIntent {
    pub mode: InstallMode,
    pub target_partition: String,
    pub target_disk_number: u32,
    pub target_partition_number: u32,
    pub image_path: String,
    pub volume_index: u32,
    pub is_system_partition: bool,
    pub selected_pe: Option<usize>,
    pub is_gho: bool,
    pub options: InstallOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcaCompatConfig {
    pub package: String,
    pub sha256: String,
    pub image_index: u32,
    pub target_build: u32,
    pub target_architecture: u16,
}

impl NativeInstallState {
    pub fn start_intent(&self) -> Result<StartInstallIntent, InstallValidationError> {
        let image_path = self.effective_image_path();
        if image_path.trim().is_empty() {
            return Err(InstallValidationError::MissingImage);
        }
        if !self.image_ready {
            return Err(InstallValidationError::ImageNotReady);
        }

        let target = self
            .target
            .as_ref()
            .ok_or(InstallValidationError::MissingTargetPartition)?;
        let target_disk_number = target
            .disk_number
            .ok_or(InstallValidationError::UnstableTargetIdentity)?;
        let target_partition_number = target
            .partition_number
            .ok_or(InstallValidationError::UnstableTargetIdentity)?;
        let is_xp_i386 = self.xp_i386_source.is_some();
        let is_gho = has_extension(&self.image_path, &["gho", "ghs"]);
        if !is_xp_i386 && !is_gho && self.selected_image.is_none() {
            return Err(InstallValidationError::MissingImageVolume);
        }
        if self
            .selected_image
            .and_then(|image| image.architecture)
            .is_some_and(|architecture| !matches!(architecture, 0 | 9))
        {
            return Err(InstallValidationError::UnsupportedImageArchitecture);
        }
        if self.custom_unattend_error.is_some() {
            return Err(InstallValidationError::InvalidCustomUnattend);
        }
        // Match the legacy UI gate: PCA selection is meaningful only when the
        // selected image supports it, boot repair is enabled and the resolved
        // target boot mode may be UEFI.  Persisted PCA preferences must not
        // block GHO/XP, explicit Legacy installs, MBR Auto installs, or an
        // install where boot repair is disabled.
        let pca_relevant = self.prefs.repair_boot
            && self.target_may_use_uefi(target)
            && self.image_supports_pca(is_gho, is_xp_i386);
        if pca_relevant {
            if self.pca_detection_pending {
                return Err(InstallValidationError::PcaDetectionPending);
            }
            if self.pca_selection_error.is_some() {
                return Err(InstallValidationError::InvalidPcaSelection);
            }
        }

        if is_xp_i386 {
            let explicit_or_known_uefi = target.style == PartitionStyle::GPT
                || self.prefs.boot_mode == BootModeSelection::UEFI;
            if explicit_or_known_uefi && !self.advanced_options_enabled {
                return Err(InstallValidationError::XpI386RequiresLegacyMbr);
            }
        }

        let mode = if self.is_pe_environment || !target.is_current_system {
            InstallMode::Direct
        } else {
            InstallMode::ViaPe
        };
        if mode == InstallMode::ViaPe {
            if !self.pe_available {
                return Err(InstallValidationError::PeUnavailable);
            }
            if self.selected_pe.is_none() {
                return Err(InstallValidationError::MissingPeSelection);
            }
        }

        let volume_index = self
            .selected_image
            .map(|image| image.volume_index)
            .unwrap_or(1);
        let is_xp = is_xp_i386
            || self
                .selected_image
                .is_some_and(|image| image.major_version == Some(5));
        let mut advanced_options = self.prefs.advanced_options.clone();
        if is_xp && !advanced_options.xp_defaults_applied {
            advanced_options.xp_inject_usb3_driver = true;
            advanced_options.xp_inject_nvme_driver = true;
            advanced_options.xp_defaults_applied = true;
        }
        let boot_pca_mode = if self.image_supports_pca(is_gho, is_xp_i386) {
            self.prefs.boot_pca_mode
        } else {
            BootPcaMode::Auto
        };
        let export_drivers = matches!(
            self.prefs.driver_action,
            DriverAction::SaveOnly | DriverAction::AutoImport
        );
        let options = InstallOptions {
            format_partition: self.prefs.format_partition,
            repair_boot: self.prefs.repair_boot,
            unattended_install: self.prefs.unattended_install,
            export_drivers,
            auto_reboot: self.prefs.auto_reboot,
            boot_mode: self.prefs.boot_mode,
            boot_pca_mode,
            advanced_options,
            driver_action: self.prefs.driver_action,
            custom_unattend_path: if self.prefs.unattended_install {
                self.custom_unattend_path.clone()
            } else {
                String::new()
            },
            is_xp,
            is_xp_i386,
            run_diskpart_scripts: self.advanced_options_enabled && self.prefs.run_diskpart_scripts,
        };

        Ok(StartInstallIntent {
            mode,
            target_partition: target.partition.clone(),
            target_disk_number,
            target_partition_number,
            image_path,
            volume_index,
            is_system_partition: target.is_current_system,
            selected_pe: self.selected_pe,
            is_gho,
            options,
        })
    }

    fn effective_image_path(&self) -> String {
        self.xp_i386_source
            .as_ref()
            .filter(|path| !path.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.image_path.clone())
    }

    fn image_supports_pca(&self, is_gho: bool, is_xp_i386: bool) -> bool {
        if is_gho || is_xp_i386 {
            return false;
        }
        self.selected_image.is_some_and(|image| {
            lr_core::pca_preflight::supports_pca_selection(image.major_version, image.architecture)
        })
    }

    fn target_may_use_uefi(&self, target: &InstallTarget) -> bool {
        match self.prefs.boot_mode {
            BootModeSelection::UEFI => true,
            BootModeSelection::Legacy => false,
            // Preserve the old resolver: an unknown partition style under
            // Auto is treated as potentially UEFI and therefore keeps the
            // fail-closed PCA check.
            BootModeSelection::Auto => target.style != PartitionStyle::MBR,
        }
    }
}

impl StartInstallIntent {
    /// Converts an already-validated intent to the existing PE INI model.
    /// The caller supplies the staged relative image path and PCA package metadata;
    /// this function does not copy either file.
    pub fn to_install_config(
        &self,
        staged_image_path: impl Into<String>,
        wim_engine: u8,
        pca: Option<&PcaCompatConfig>,
    ) -> InstallConfig {
        let advanced = &self.options.advanced_options;
        let pca = pca.cloned().unwrap_or_default();
        InstallConfig {
            session_id: String::new(),
            unattended: self.options.unattended_install,
            restore_drivers: self.options.export_drivers,
            driver_action_mode: InstallConfig::driver_action_to_mode(self.options.driver_action),
            auto_reboot: self.options.auto_reboot,
            original_guid: String::new(),
            volume_index: self.volume_index,
            target_partition: self.target_partition.clone(),
            image_path: staged_image_path.into(),
            is_gho: self.is_gho,
            remove_shortcut_arrow: advanced.remove_shortcut_arrow,
            restore_classic_context_menu: advanced.restore_classic_context_menu,
            bypass_nro: advanced.bypass_nro,
            disable_windows_update: advanced.disable_windows_update,
            disable_windows_defender: advanced.disable_windows_defender,
            disable_reserved_storage: advanced.disable_reserved_storage,
            disable_uac: advanced.disable_uac,
            disable_device_encryption: advanced.disable_device_encryption,
            remove_uwp_apps: advanced.remove_uwp_apps,
            import_storage_controller_drivers: advanced.import_storage_controller_drivers,
            custom_username: if advanced.custom_username {
                advanced.username.clone()
            } else {
                String::new()
            },
            volume_label: if advanced.custom_volume_label {
                advanced.volume_label.clone()
            } else {
                String::new()
            },
            custom_unattend_path: self.options.custom_unattend_path.clone(),
            win7_uefi_patch: advanced.win7_uefi_patch,
            win7_inject_usb3_driver: advanced.win7_inject_usb3_driver,
            win7_inject_nvme_driver: advanced.win7_inject_nvme_driver,
            win7_fix_acpi_bsod: advanced.win7_fix_acpi_bsod,
            win7_fix_storage_bsod: advanced.win7_fix_storage_bsod,
            wim_engine,
            is_xp: self.options.is_xp,
            is_xp_i386: self.options.is_xp_i386,
            xp_source_arch: String::new(),
            xp_inject_usb3_driver: advanced.xp_inject_usb3_driver,
            xp_inject_nvme_driver: advanced.xp_inject_nvme_driver,
            run_diskpart_scripts: self.options.run_diskpart_scripts,
            boot_mode: self.options.boot_mode.as_u8(),
            boot_pca_mode: self.options.boot_pca_mode,
            pca_compat_package: pca.package,
            pca_compat_sha256: pca.sha256,
            pca_compat_image_index: pca.image_index,
            pca_compat_target_build: pca.target_build,
            pca_compat_target_architecture: pca.target_architecture,
        }
    }
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> NativeInstallState {
        NativeInstallState {
            image_path: "D:\\install.wim".to_string(),
            image_ready: true,
            selected_image: Some(SelectedImageMetadata {
                volume_index: 3,
                major_version: Some(10),
                architecture: Some(9),
            }),
            xp_i386_source: None,
            target: Some(InstallTarget {
                partition: "E:".to_string(),
                disk_number: Some(1),
                partition_number: Some(2),
                style: PartitionStyle::GPT,
                is_current_system: false,
                has_windows: false,
            }),
            is_pe_environment: false,
            pe_available: false,
            selected_pe: None,
            custom_unattend_path: String::new(),
            custom_unattend_error: None,
            pca_detection_pending: false,
            pca_selection_error: None,
            advanced_options_enabled: false,
            prefs: InstallPrefs::default(),
        }
    }

    #[test]
    fn non_system_target_is_direct() {
        let intent = base_state().start_intent().unwrap();
        assert_eq!(intent.mode, InstallMode::Direct);
        assert_eq!(intent.volume_index, 3);
    }

    #[test]
    fn current_system_requires_selected_pe() {
        let mut state = base_state();
        state.target.as_mut().unwrap().is_current_system = true;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::PeUnavailable
        );
        state.pe_available = true;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::MissingPeSelection
        );
        state.selected_pe = Some(0);
        assert_eq!(state.start_intent().unwrap().mode, InstallMode::ViaPe);
    }

    #[test]
    fn pe_environment_always_uses_direct_mode() {
        let mut state = base_state();
        state.target.as_mut().unwrap().is_current_system = true;
        state.is_pe_environment = true;
        assert_eq!(state.start_intent().unwrap().mode, InstallMode::Direct);
    }

    #[test]
    fn gho_uses_index_one_and_disables_pca_selection() {
        let mut state = base_state();
        state.image_path = "D:\\backup.GHS".to_string();
        state.selected_image = None;
        state.prefs.boot_pca_mode = BootPcaMode::Pca2023;
        let intent = state.start_intent().unwrap();
        assert!(intent.is_gho);
        assert_eq!(intent.volume_index, 1);
        assert_eq!(intent.options.boot_pca_mode, BootPcaMode::Auto);
    }

    #[test]
    fn xp_i386_current_system_routes_through_selected_pe() {
        let mut state = base_state();
        state.xp_i386_source = Some("F:\\I386".to_string());
        state.selected_image = None;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::XpI386RequiresLegacyMbr
        );

        state.target.as_mut().unwrap().style = PartitionStyle::MBR;
        state.target.as_mut().unwrap().is_current_system = true;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::PeUnavailable
        );
        state.pe_available = true;
        state.selected_pe = Some(0);
        let intent = state.start_intent().unwrap();
        assert_eq!(intent.mode, InstallMode::ViaPe);
        assert!(intent.options.is_xp_i386);
        assert!(intent.options.advanced_options.xp_inject_usb3_driver);
        assert!(intent.options.advanced_options.xp_inject_nvme_driver);
    }

    #[test]
    fn unsupported_architecture_is_rejected_before_dispatch() {
        let mut state = base_state();
        state.selected_image.as_mut().unwrap().architecture = Some(12);
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::UnsupportedImageArchitecture
        );
    }

    #[test]
    fn install_intent_requires_a_stable_disk_and_partition_identity() {
        let mut state = base_state();
        state.target.as_mut().unwrap().disk_number = None;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::UnstableTargetIdentity
        );

        state.target.as_mut().unwrap().disk_number = Some(1);
        state.target.as_mut().unwrap().partition_number = None;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::UnstableTargetIdentity
        );
    }

    #[test]
    fn irrelevant_pca_state_does_not_block_non_pca_install_paths() {
        let mut state = base_state();
        state.pca_detection_pending = true;
        state.pca_selection_error = Some("stale firmware result".to_string());

        state.image_path = "D:\\backup.gho".to_string();
        state.selected_image = None;
        assert!(state.start_intent().is_ok());

        state.image_path = "D:\\install.wim".to_string();
        state.selected_image = base_state().selected_image;
        state.prefs.boot_mode = BootModeSelection::Legacy;
        assert!(state.start_intent().is_ok());

        state.prefs.boot_mode = BootModeSelection::Auto;
        state.target.as_mut().unwrap().style = PartitionStyle::MBR;
        assert!(state.start_intent().is_ok());

        state.target.as_mut().unwrap().style = PartitionStyle::GPT;
        state.prefs.repair_boot = false;
        assert!(state.start_intent().is_ok());
    }

    #[test]
    fn relevant_pca_state_remains_fail_closed_for_gpt_and_unknown_auto_targets() {
        for style in [PartitionStyle::GPT, PartitionStyle::Unknown] {
            let mut state = base_state();
            state.target.as_mut().unwrap().style = style;
            state.pca_detection_pending = true;
            assert_eq!(
                state.start_intent().unwrap_err(),
                InstallValidationError::PcaDetectionPending
            );

            state.pca_detection_pending = false;
            state.pca_selection_error = Some("firmware rejects selection".to_string());
            assert_eq!(
                state.start_intent().unwrap_err(),
                InstallValidationError::InvalidPcaSelection
            );
        }
    }

    #[test]
    fn install_config_conversion_preserves_existing_fields() {
        let mut state = base_state();
        state.prefs.driver_action = DriverAction::AutoImport;
        state.prefs.advanced_options.custom_username = true;
        state.prefs.advanced_options.username = "LetRecovery".to_string();
        state.prefs.run_diskpart_scripts = true;
        state.advanced_options_enabled = true;
        let intent = state.start_intent().unwrap();
        let pca = PcaCompatConfig {
            package: "pca_compat\\package.wim".to_string(),
            sha256: "a".repeat(64),
            image_index: 1,
            target_build: 26_100,
            target_architecture: 9,
        };
        let config = intent.to_install_config("images\\install.wim", 1, Some(&pca));
        assert_eq!(config.driver_action_mode, 2);
        assert_eq!(config.custom_username, "LetRecovery");
        assert!(config.run_diskpart_scripts);
        assert!(!config.is_xp_i386);
        assert_eq!(config.boot_pca_mode, BootPcaMode::Auto);
        assert_eq!(config.pca_compat_target_build, 26_100);
    }
}
