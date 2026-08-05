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
    pub disk_size_bytes: Option<u64>,
    pub partition_offset_bytes: Option<u64>,
    pub partition_size_bytes: Option<u64>,
    /// Bus type captured from the same physical disk identity as this target snapshot.
    pub disk_bus_type: Option<lr_core::windows_storage::DiskBusType>,
    pub style: PartitionStyle,
    pub is_current_system: bool,
    pub has_windows: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectedImageMetadata {
    pub volume_index: u32,
    pub major_version: Option<u16>,
    pub minor_version: Option<u16>,
    pub build: Option<u32>,
    pub architecture: Option<u16>,
}

impl SelectedImageMetadata {
    pub const fn is_windows_7(self) -> bool {
        matches!((self.major_version, self.minor_version), (Some(6), Some(1)))
    }
}

/// Built-in Windows 7 compatibility-driver defaults.
///
/// USB 3.x is enabled for every positively identified Windows 7 image. NVMe is
/// enabled only when the selected physical target returned `BusTypeNvme`; an
/// unavailable or abstracted RAID/VMD bus query remains fail-closed.
pub fn windows7_driver_defaults(
    image: Option<SelectedImageMetadata>,
    target_bus: Option<lr_core::windows_storage::DiskBusType>,
) -> (bool, bool) {
    let is_windows_7 = image.is_some_and(SelectedImageMetadata::is_windows_7);
    (
        is_windows_7,
        is_windows_7
            && image.and_then(|metadata| metadata.architecture) == Some(9)
            && target_bus == Some(lr_core::windows_storage::DiskBusType::Nvme),
    )
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
    pub custom_unattend_path: String,
    pub custom_unattend_error: Option<String>,
    /// True while a device-change inventory refresh is queued or running. The cached target
    /// identity must not be dispatched until the refresh has accepted a current snapshot.
    pub partition_refresh_pending: bool,
    pub partition_refresh_error: Option<String>,
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
    InvalidCustomUnattend,
    BuiltInAdministratorRequiresUnattended,
    BuiltInAdministratorUnsupportedSource,
    BuiltInAdministratorConflictsWithCustomUnattend,
    ConflictingAdministratorOptions,
    InvalidCustomUsername,
    InvalidBuiltInAdministratorName,
    InvalidBuiltInAdministratorPassword,
    PartitionRefreshPending,
    PartitionRefreshFailed,
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
            Self::InvalidCustomUnattend => crate::tr!("自定义无人值守文件无效。"),
            Self::BuiltInAdministratorRequiresUnattended => {
                crate::tr!("启用内置 Administrator 账户需要同时启用无人值守安装。")
            }
            Self::BuiltInAdministratorUnsupportedSource => {
                crate::tr!(
                    "内置 Administrator 账户选项仅支持 Windows 7 或更高版本的 WIM、ESD、SWM 镜像。"
                )
            }
            Self::BuiltInAdministratorConflictsWithCustomUnattend => {
                crate::tr!("启用内置 Administrator 账户时不能同时使用自定义无人值守文件。")
            }
            Self::ConflictingAdministratorOptions => {
                crate::tr!("“自定义用户名”和“启用内置 Administrator 账户”不能同时启用。")
            }
            Self::InvalidCustomUsername => crate::tr!(
                "自定义用户名无效：请使用普通本地账户名，不能使用 SYSTEM、TrustedInstaller 等系统保留账户，且不得包含 Windows 禁止字符。"
            ),
            Self::InvalidBuiltInAdministratorName => {
                crate::tr!("内置 Administrator 账户名无效：不能为空、不得超过 20 个字符或包含 Windows 禁止字符。")
            }
            Self::InvalidBuiltInAdministratorPassword => {
                crate::tr!(
                    "请为内置 Administrator 账户设置有效密码（最长 127 个字符，不能包含换行）。"
                )
            }
            Self::PartitionRefreshPending => crate::tr!("正在刷新分区信息，请稍候。"),
            Self::PartitionRefreshFailed => crate::tr!("刷新分区信息失败，请手动刷新后重试。"),
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

/// Runtime options assembled from the validated native install-page controls.
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
    pub target_disk_size_bytes: u64,
    pub target_partition_offset_bytes: u64,
    pub target_partition_size_bytes: u64,
    pub image_path: String,
    pub volume_index: u32,
    pub is_system_partition: bool,
    pub pe_index: Option<usize>,
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
        let target_disk_size_bytes = target
            .disk_size_bytes
            .ok_or(InstallValidationError::UnstableTargetIdentity)?;
        let target_partition_offset_bytes = target
            .partition_offset_bytes
            .ok_or(InstallValidationError::UnstableTargetIdentity)?;
        let target_partition_size_bytes = target
            .partition_size_bytes
            .ok_or(InstallValidationError::UnstableTargetIdentity)?;
        let is_xp_i386 = self.xp_i386_source.is_some();
        let is_gho = has_extension(&self.image_path, &["gho", "ghs"]);
        let builtin = &self.prefs.advanced_options.builtin_administrator;
        if self.prefs.advanced_options.custom_username
            && lr_core::unattend_account::validate_unattended_local_account_name(
                &self.prefs.advanced_options.username,
            )
            .is_err()
        {
            return Err(InstallValidationError::InvalidCustomUsername);
        }
        if builtin.enabled {
            if !self.prefs.unattended_install {
                return Err(InstallValidationError::BuiltInAdministratorRequiresUnattended);
            }
            if is_xp_i386
                || is_gho
                || self
                    .selected_image
                    .is_some_and(|image| image.major_version == Some(5))
            {
                return Err(InstallValidationError::BuiltInAdministratorUnsupportedSource);
            }
            if !self.custom_unattend_path.trim().is_empty() {
                return Err(
                    InstallValidationError::BuiltInAdministratorConflictsWithCustomUnattend,
                );
            }
            if self.prefs.advanced_options.custom_username {
                return Err(InstallValidationError::ConflictingAdministratorOptions);
            }
            if let Err(error) = builtin.validate() {
                use lr_core::unattend_account::BuiltInAdministratorValidationError as Error;
                return Err(match error {
                    Error::MissingAccountName
                    | Error::AccountNameTooLong
                    | Error::InvalidAccountName
                    | Error::ReservedAccountName => {
                        InstallValidationError::InvalidBuiltInAdministratorName
                    }
                    Error::MissingPassword | Error::PasswordTooLong | Error::InvalidPassword => {
                        InstallValidationError::InvalidBuiltInAdministratorPassword
                    }
                });
            }
        }
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
        if self.partition_refresh_error.is_some() {
            return Err(InstallValidationError::PartitionRefreshFailed);
        }
        if self.partition_refresh_pending {
            return Err(InstallValidationError::PartitionRefreshPending);
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
        if mode == InstallMode::ViaPe && !self.pe_available {
            return Err(InstallValidationError::PeUnavailable);
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
        advanced_options.retain_supported_for_target(
            crate::core::ui_state::AdvancedOptionCapabilities::for_target(
                self.selected_image.and_then(|image| image.major_version),
                self.selected_image.and_then(|image| image.minor_version),
                self.selected_image.and_then(|image| image.build),
                is_xp_i386,
            ),
        );
        // Windows 7 compatibility payloads are bundled, locked and selected by hardware policy.
        // They are not user-supplied advanced options: USB3 is considered for every identified
        // Windows 7 image, while the NVMe hotfix pair is allowed only for x64 plus a positively
        // identified native NVMe target. The historical processor-power workaround remains an
        // explicit Windows 7 choice; the broad storage registry hack and UefiSeven stay retired.
        let (win7_usb3, win7_nvme) =
            windows7_driver_defaults(self.selected_image, target.disk_bus_type);
        advanced_options.win7_inject_usb3_driver = win7_usb3;
        advanced_options.win7_usb3_driver_path.clear();
        advanced_options.win7_inject_nvme_driver = win7_nvme;
        advanced_options.win7_nvme_driver_path.clear();
        advanced_options.win7_fix_storage_bsod = false;
        advanced_options.win7_uefi_patch = false;
        let boot_pca_mode = if self.image_supports_pca(is_gho, is_xp_i386) {
            self.prefs.boot_pca_mode
        } else {
            BootPcaMode::Auto
        };
        // A freshly partitioned target has no host DriverStore to preserve. Scheduling an offline
        // DISM export in that state makes a valid clean install fail before formatting/image apply.
        let export_drivers = target.has_windows
            && matches!(
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
            // Retain the serialized preference for upgrade compatibility, but arbitrary scripts
            // are no longer executable after the storage paths moved to parameterized WinAPI.
            run_diskpart_scripts: false,
        };

        Ok(StartInstallIntent {
            mode,
            target_partition: target.partition.clone(),
            target_disk_number,
            target_partition_number,
            target_disk_size_bytes,
            target_partition_offset_bytes,
            target_partition_size_bytes,
            image_path,
            volume_index,
            is_system_partition: target.is_current_system,
            pe_index: (mode == InstallMode::ViaPe).then_some(0),
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
            format_partition: self.options.format_partition,
            repair_boot: self.options.repair_boot,
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
            builtin_administrator: advanced.builtin_administrator.clone(),
            volume_label: if advanced.custom_volume_label {
                advanced.volume_label.clone()
            } else {
                String::new()
            },
            custom_unattend_path: self.options.custom_unattend_path.clone(),
            win7_uefi_patch: advanced.win7_uefi_patch,
            win7_inject_usb3_driver: advanced.win7_inject_usb3_driver,
            win7_inject_nvme_driver: advanced.win7_inject_nvme_driver,
            // Keep the historical processor-power workaround available to both direct and PE
            // installs. It remains opt-in and is filtered out for every non-Windows 7 target.
            win7_fix_acpi_bsod: advanced.win7_fix_acpi_bsod,
            win7_fix_storage_bsod: false,
            wim_engine,
            is_xp: self.options.is_xp,
            is_xp_i386: self.options.is_xp_i386,
            xp_source_arch: String::new(),
            xp_inject_usb3_driver: advanced.xp_inject_usb3_driver,
            xp_inject_nvme_driver: advanced.xp_inject_nvme_driver,
            // The field remains serialized only so old PE/config readers stay compatible.
            // Typed WinAPI storage operations replace arbitrary partition scripts.
            run_diskpart_scripts: false,
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

    fn image(major: u16, minor: u16) -> SelectedImageMetadata {
        SelectedImageMetadata {
            volume_index: 1,
            major_version: Some(major),
            minor_version: Some(minor),
            build: (major >= 10).then_some(26_100),
            architecture: Some(9),
        }
    }

    #[test]
    fn win7_usb3_is_defaulted_even_when_target_bus_is_unknown() {
        assert_eq!(
            windows7_driver_defaults(Some(image(6, 1)), None),
            (true, false)
        );
        assert_eq!(
            windows7_driver_defaults(
                Some(image(6, 1)),
                Some(lr_core::windows_storage::DiskBusType::Other)
            ),
            (true, false)
        );
    }

    #[test]
    fn win7_nvme_is_defaulted_only_for_an_explicit_nvme_bus() {
        assert_eq!(
            windows7_driver_defaults(
                Some(image(6, 1)),
                Some(lr_core::windows_storage::DiskBusType::Nvme)
            ),
            (true, true)
        );
        assert_eq!(
            windows7_driver_defaults(
                Some(image(10, 0)),
                Some(lr_core::windows_storage::DiskBusType::Nvme)
            ),
            (false, false)
        );
        let mut x86 = image(6, 1);
        x86.architecture = Some(0);
        assert_eq!(
            windows7_driver_defaults(Some(x86), Some(lr_core::windows_storage::DiskBusType::Nvme)),
            (true, false)
        );
    }

    fn base_state() -> NativeInstallState {
        NativeInstallState {
            image_path: "D:\\install.wim".to_string(),
            image_ready: true,
            selected_image: Some(SelectedImageMetadata {
                volume_index: 3,
                major_version: Some(10),
                minor_version: Some(0),
                build: Some(26_100),
                architecture: Some(9),
            }),
            xp_i386_source: None,
            target: Some(InstallTarget {
                partition: "E:".to_string(),
                disk_number: Some(1),
                partition_number: Some(2),
                disk_size_bytes: Some(1_000_000_000_000),
                partition_offset_bytes: Some(1_048_576),
                partition_size_bytes: Some(500_000_000_000),
                disk_bus_type: Some(lr_core::windows_storage::DiskBusType::Other),
                style: PartitionStyle::GPT,
                is_current_system: false,
                has_windows: false,
            }),
            is_pe_environment: false,
            pe_available: false,
            custom_unattend_path: String::new(),
            custom_unattend_error: None,
            partition_refresh_pending: false,
            partition_refresh_error: None,
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
        assert!(!intent.options.export_drivers);
    }

    #[test]
    fn windows_7_install_intent_cannot_carry_hidden_modern_windows_options() {
        let mut state = base_state();
        state.selected_image = Some(image(6, 1));
        state.prefs.advanced_options.restore_classic_context_menu = true;
        state.prefs.advanced_options.disable_windows_defender = true;
        state.prefs.advanced_options.remove_uwp_apps = true;
        state
            .prefs
            .advanced_options
            .import_storage_controller_drivers = true;
        state.prefs.advanced_options.win7_inject_usb3_driver = true;

        let options = state.start_intent().unwrap().options.advanced_options;
        assert!(options.remove_shortcut_arrow);
        assert!(!options.restore_classic_context_menu);
        assert!(!options.bypass_nro);
        assert!(!options.disable_windows_defender);
        assert!(!options.disable_reserved_storage);
        assert!(!options.disable_device_encryption);
        assert!(!options.remove_uwp_apps);
        assert!(!options.import_storage_controller_drivers);
        assert!(options.win7_inject_usb3_driver);
    }

    #[test]
    fn windows_7_driver_policy_overrides_legacy_user_choices() {
        let mut state = base_state();
        state.selected_image = Some(image(6, 1));
        state.target.as_mut().unwrap().disk_bus_type =
            Some(lr_core::windows_storage::DiskBusType::Nvme);
        state.prefs.advanced_options.win7_inject_usb3_driver = false;
        state.prefs.advanced_options.win7_usb3_driver_path = "D:\\custom-usb3".to_string();
        state.prefs.advanced_options.win7_inject_nvme_driver = false;
        state.prefs.advanced_options.win7_nvme_driver_path = "D:\\custom-nvme".to_string();
        state.prefs.advanced_options.win7_fix_acpi_bsod = true;
        state.prefs.advanced_options.win7_fix_storage_bsod = true;
        state.prefs.advanced_options.win7_uefi_patch = true;

        let options = state.start_intent().unwrap().options.advanced_options;
        assert!(options.win7_inject_usb3_driver);
        assert!(options.win7_inject_nvme_driver);
        assert!(options.win7_usb3_driver_path.is_empty());
        assert!(options.win7_nvme_driver_path.is_empty());
        assert!(options.win7_fix_acpi_bsod);
        assert!(!options.win7_fix_storage_bsod);
        assert!(!options.win7_uefi_patch);
    }

    #[test]
    fn windows_7_nvme_hotfix_is_not_guessed_for_unknown_or_raid_bus() {
        for bus in [None, Some(lr_core::windows_storage::DiskBusType::Other)] {
            let mut state = base_state();
            state.selected_image = Some(image(6, 1));
            state.target.as_mut().unwrap().disk_bus_type = bus;
            state.prefs.advanced_options.win7_inject_nvme_driver = true;
            let options = state.start_intent().unwrap().options.advanced_options;
            assert!(options.win7_inject_usb3_driver);
            assert!(!options.win7_inject_nvme_driver);
        }
    }

    #[test]
    fn host_drivers_are_exported_only_when_the_target_contains_windows() {
        let mut state = base_state();
        state.prefs.driver_action = DriverAction::AutoImport;
        assert!(!state.start_intent().unwrap().options.export_drivers);

        state.target.as_mut().unwrap().has_windows = true;
        assert!(state.start_intent().unwrap().options.export_drivers);
    }

    #[test]
    fn reserved_custom_username_is_rejected_before_install_dispatch() {
        for reserved in ["SYSTEM", "TrustedInstaller", "DWM-1"] {
            let mut state = base_state();
            state.prefs.advanced_options.custom_username = true;
            state.prefs.advanced_options.username = reserved.to_string();
            assert_eq!(
                state.start_intent().unwrap_err(),
                InstallValidationError::InvalidCustomUsername,
                "{reserved}"
            );
        }

        let mut state = base_state();
        state.prefs.advanced_options.username = "Terry".to_string();
        assert!(state.start_intent().is_ok());
    }

    #[test]
    fn current_system_automatically_uses_first_available_pe() {
        let mut state = base_state();
        state.target.as_mut().unwrap().is_current_system = true;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::PeUnavailable
        );
        state.pe_available = true;
        let intent = state.start_intent().unwrap();
        assert_eq!(intent.mode, InstallMode::ViaPe);
        assert_eq!(intent.pe_index, Some(0));
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
    fn xp_i386_current_system_routes_through_first_available_pe() {
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
        let intent = state.start_intent().unwrap();
        assert_eq!(intent.mode, InstallMode::ViaPe);
        assert_eq!(intent.pe_index, Some(0));
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

        state.target.as_mut().unwrap().partition_number = Some(2);
        state.target.as_mut().unwrap().partition_offset_bytes = None;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::UnstableTargetIdentity
        );
    }

    #[test]
    fn queued_or_running_partition_refresh_blocks_stale_target_dispatch() {
        let mut state = base_state();
        state.partition_refresh_pending = true;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::PartitionRefreshPending
        );

        state.partition_refresh_pending = false;
        assert!(state.start_intent().is_ok());
    }

    #[test]
    fn failed_partition_refresh_remains_fail_closed_until_a_successful_retry() {
        let mut state = base_state();
        state.partition_refresh_error = Some("inventory unavailable".to_string());
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::PartitionRefreshFailed
        );

        state.partition_refresh_error = None;
        assert!(state.start_intent().is_ok());
    }

    #[test]
    fn selected_boot_mode_survives_intent_and_pe_config_serialization() {
        for (selection, expected) in [
            (BootModeSelection::Auto, 0),
            (BootModeSelection::UEFI, 1),
            (BootModeSelection::Legacy, 2),
        ] {
            let mut state = base_state();
            state.prefs.boot_mode = selection;
            let intent = state.start_intent().unwrap();
            assert_eq!(intent.options.boot_mode, selection);
            let config = intent.to_install_config("images\\install.wim", 1, None);
            assert_eq!(config.boot_mode, expected);
        }
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
    fn install_config_conversion_disables_legacy_partition_scripts() {
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
        assert_eq!(config.format_partition, state.prefs.format_partition);
        assert_eq!(config.repair_boot, state.prefs.repair_boot);
        assert!(!config.run_diskpart_scripts);
        assert!(!config.is_xp_i386);
        assert_eq!(config.boot_pca_mode, BootPcaMode::Auto);
        assert_eq!(config.pca_compat_target_build, 26_100);
    }

    #[test]
    fn builtin_administrator_requires_safe_unattended_wim_flow() {
        let mut state = base_state();
        state.prefs.advanced_options.builtin_administrator.enabled = true;
        state.prefs.advanced_options.builtin_administrator.password = "test-password".into();
        state.prefs.unattended_install = false;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::BuiltInAdministratorRequiresUnattended
        );

        state.prefs.unattended_install = true;
        state.custom_unattend_path = "D:\\autounattend.xml".to_string();
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::BuiltInAdministratorConflictsWithCustomUnattend
        );

        state.custom_unattend_path.clear();
        state.prefs.advanced_options.custom_username = true;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::ConflictingAdministratorOptions
        );

        state.prefs.advanced_options.custom_username = false;
        state.image_path = "D:\\backup.gho".to_string();
        state.selected_image = None;
        assert_eq!(
            state.start_intent().unwrap_err(),
            InstallValidationError::BuiltInAdministratorUnsupportedSource
        );
    }

    #[test]
    fn builtin_administrator_secret_reaches_only_the_install_session_config() {
        let mut state = base_state();
        // Runtime defaults select the mutually exclusive ordinary-user mode. This fixture
        // intentionally exercises the Administrator path, so switch modes exactly as the UI
        // radio button does before enabling RID-500 configuration.
        state.prefs.advanced_options.custom_username = false;
        state.prefs.advanced_options.builtin_administrator.enabled = true;
        state
            .prefs
            .advanced_options
            .builtin_administrator
            .account_name = "LocalAdmin".to_string();
        state.prefs.advanced_options.builtin_administrator.password = "one-time-secret".into();
        state
            .prefs
            .advanced_options
            .builtin_administrator
            .auto_logon = true;

        let intent = state.start_intent().unwrap();
        let config = intent.to_install_config("images\\install.wim", 1, None);
        assert!(config.builtin_administrator.enabled);
        assert_eq!(config.builtin_administrator.account_name, "LocalAdmin");
        assert_eq!(
            config.builtin_administrator.password.expose_secret(),
            "one-time-secret"
        );
        assert!(config.builtin_administrator.auto_logon);
    }
}
