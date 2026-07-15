//! Side-effect-free controller for the native easy-mode install page.

use std::path::{Path, PathBuf};

use crate::core::ui_state::{AdvancedOptionsData, DriverAction, InstallPrefs};
use crate::download::config::{EasyModeConfig, EasyModeSystem};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EasyLogoSource {
    EmbeddedWindows10,
    EmbeddedWindows11,
    Remote(String),
}

impl EasyLogoSource {
    pub fn from_config(value: &str) -> Option<Self> {
        match value.trim() {
            "" => None,
            "LOGO_WINDOWS10" => Some(Self::EmbeddedWindows10),
            "LOGO_WINDOWS11" => Some(Self::EmbeddedWindows11),
            value => Some(Self::Remote(value.to_string())),
        }
    }

    pub fn display_hint(&self) -> &str {
        match self {
            Self::EmbeddedWindows10 => "Windows 10",
            Self::EmbeddedWindows11 => "Windows 11",
            Self::Remote(_) => "Logo",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EasyVolumeEntry {
    pub number: u32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EasySystemEntry {
    pub name: String,
    pub logo: Option<EasyLogoSource>,
    /// Kept byte-for-byte from the server field; dispatch validates it later.
    pub download_url: String,
    pub volumes: Vec<EasyVolumeEntry>,
}

impl EasySystemEntry {
    fn from_config(name: String, system: EasyModeSystem) -> Self {
        Self {
            name,
            logo: EasyLogoSource::from_config(&system.os_logo),
            download_url: system.os_download,
            volumes: system
                .volume
                .into_iter()
                .map(|volume| EasyVolumeEntry {
                    number: volume.number,
                    name: volume.name,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EasyModeView {
    pub enabled: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub settings_tip_visible: bool,
    pub systems: Vec<String>,
    pub selected_system: Option<usize>,
    pub volumes: Vec<String>,
    pub selected_volume: Option<usize>,
    pub logo: Option<EasyLogoSource>,
    pub description: String,
    pub can_install: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EasyModeAction {
    SetEnabled(bool),
    DismissSettingsTip,
    SelectSystem(usize),
    SelectVolume(usize),
}

#[derive(Clone, Debug)]
pub struct StartEasyInstallIntent {
    pub system_name: String,
    pub download_url: String,
    pub filename: String,
    pub download_directory: PathBuf,
    pub download_path: PathBuf,
    pub volume_number: u32,
    pub system_partition_index: usize,
    pub download_then_install: bool,
    pub easy_mode_auto_install: bool,
    pub prefs: InstallPrefs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EasyModeValidationError {
    Disabled,
    CatalogueLoading,
    CatalogueUnavailable,
    NoSystems,
    MissingSystemSelection,
    MissingVolumeSelection,
    MissingDownloadUrl,
    MissingSystemPartition,
}

impl std::fmt::Display for EasyModeValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Disabled => "easy mode is disabled",
            Self::CatalogueLoading => "the easy-mode catalogue is still loading",
            Self::CatalogueUnavailable => "the easy-mode catalogue is unavailable",
            Self::NoSystems => "the easy-mode catalogue contains no systems",
            Self::MissingSystemSelection => "no easy-mode system is selected",
            Self::MissingVolumeSelection => "no easy-mode image volume is selected",
            Self::MissingDownloadUrl => "the selected system has no download URL",
            Self::MissingSystemPartition => "the current system partition was not found",
        })
    }
}

impl std::error::Error for EasyModeValidationError {}

#[derive(Clone, Debug)]
pub struct NativeEasyModeController {
    enabled: bool,
    loading: bool,
    catalogue_available: bool,
    settings_tip_dismissed: bool,
    systems: Vec<EasySystemEntry>,
    selected_system: Option<usize>,
    selected_volume: Option<usize>,
}

impl NativeEasyModeController {
    pub fn new(enabled: bool, settings_tip_dismissed: bool) -> Self {
        Self {
            enabled,
            loading: false,
            catalogue_available: false,
            settings_tip_dismissed,
            systems: Vec::new(),
            selected_system: None,
            selected_volume: None,
        }
    }

    pub fn set_catalogue(&mut self, config: Option<&EasyModeConfig>, loading: bool) {
        self.loading = loading;
        self.catalogue_available = config.is_some();
        self.systems = config
            .map(EasyModeConfig::get_systems)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, system)| EasySystemEntry::from_config(name, system))
            .collect();
        if self
            .selected_system
            .is_some_and(|index| index >= self.systems.len())
        {
            self.selected_system = None;
            self.selected_volume = None;
        }
        self.ensure_default_selection();
    }

    fn ensure_default_selection(&mut self) {
        if !self.enabled || self.loading || !self.catalogue_available {
            return;
        }
        if self.selected_system.is_none() && !self.systems.is_empty() {
            self.selected_system = Some(0);
        }
        let Some(system) = self
            .selected_system
            .and_then(|index| self.systems.get(index))
        else {
            self.selected_volume = None;
            return;
        };
        if self
            .selected_volume
            .is_none_or(|index| index >= system.volumes.len())
        {
            self.selected_volume = (!system.volumes.is_empty()).then_some(0);
        }
    }

    pub fn apply(&mut self, action: EasyModeAction) {
        match action {
            EasyModeAction::SetEnabled(enabled) => {
                self.enabled = enabled;
                self.ensure_default_selection();
            }
            EasyModeAction::DismissSettingsTip => self.settings_tip_dismissed = true,
            EasyModeAction::SelectSystem(index) if index < self.systems.len() => {
                self.selected_system = Some(index);
                self.selected_volume = (!self.systems[index].volumes.is_empty()).then_some(0);
            }
            EasyModeAction::SelectVolume(index)
                if self
                    .selected_system
                    .and_then(|system| self.systems.get(system))
                    .is_some_and(|system| index < system.volumes.len()) =>
            {
                self.selected_volume = Some(index);
            }
            EasyModeAction::SelectSystem(_) | EasyModeAction::SelectVolume(_) => {}
        }
    }

    pub fn view(&self) -> EasyModeView {
        let selected = self
            .selected_system
            .and_then(|index| self.systems.get(index));
        let selected_volume = selected.and_then(|system| {
            self.selected_volume
                .and_then(|index| system.volumes.get(index))
        });
        let error = if self.loading {
            None
        } else if !self.catalogue_available {
            Some(crate::tr!("无法获取系统列表，请检查网络连接后重启程序"))
        } else if self.systems.is_empty() {
            Some(crate::tr!("暂无可用的系统镜像"))
        } else {
            None
        };
        let description = match (selected, selected_volume) {
            (Some(system), Some(volume)) => format!("{} - {}", system.name, volume.name),
            (Some(system), None) => format!("{} - {}", system.name, crate::tr!("请先选择版本")),
            _ if self.loading => crate::tr!("正在加载系统列表..."),
            _ => crate::tr!("请选择要安装的系统："),
        };
        EasyModeView {
            enabled: self.enabled,
            loading: self.loading,
            error,
            settings_tip_visible: !self.settings_tip_dismissed,
            systems: self
                .systems
                .iter()
                .map(|system| system.name.clone())
                .collect(),
            selected_system: self.selected_system,
            volumes: selected
                .map(|system| {
                    system
                        .volumes
                        .iter()
                        .map(|volume| volume.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            selected_volume: self.selected_volume,
            logo: selected.and_then(|system| system.logo.clone()),
            description,
            can_install: self.enabled && selected_volume.is_some(),
        }
    }

    pub fn start_install_intent(
        &self,
        system_partition_index: Option<usize>,
        download_directory: &Path,
        current_username: Option<&str>,
    ) -> Result<StartEasyInstallIntent, EasyModeValidationError> {
        if !self.enabled {
            return Err(EasyModeValidationError::Disabled);
        }
        if self.loading {
            return Err(EasyModeValidationError::CatalogueLoading);
        }
        if !self.catalogue_available {
            return Err(EasyModeValidationError::CatalogueUnavailable);
        }
        if self.systems.is_empty() {
            return Err(EasyModeValidationError::NoSystems);
        }
        let system = self
            .selected_system
            .and_then(|index| self.systems.get(index))
            .ok_or(EasyModeValidationError::MissingSystemSelection)?;
        let volume = self
            .selected_volume
            .and_then(|index| system.volumes.get(index))
            .ok_or(EasyModeValidationError::MissingVolumeSelection)?;
        if system.download_url.trim().is_empty() {
            return Err(EasyModeValidationError::MissingDownloadUrl);
        }
        let system_partition_index =
            system_partition_index.ok_or(EasyModeValidationError::MissingSystemPartition)?;
        let filename = system
            .download_url
            .split('/')
            .next_back()
            .unwrap_or("system.esd")
            .to_string();
        let username = current_username
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("User");
        let advanced_options = AdvancedOptionsData {
            bypass_nro: true,
            remove_uwp_apps: true,
            import_storage_controller_drivers: true,
            custom_volume_label: true,
            volume_label: "OS".to_string(),
            custom_username: true,
            username: username.to_string(),
            ..Default::default()
        };
        let prefs = InstallPrefs {
            format_partition: true,
            repair_boot: true,
            unattended_install: true,
            driver_action: DriverAction::AutoImport,
            auto_reboot: true,
            advanced_options,
            ..Default::default()
        };
        Ok(StartEasyInstallIntent {
            system_name: system.name.clone(),
            download_url: system.download_url.clone(),
            download_path: download_directory.join(&filename),
            filename,
            download_directory: download_directory.to_path_buf(),
            volume_number: volume.number,
            system_partition_index,
            download_then_install: true,
            easy_mode_auto_install: true,
            prefs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::config::{EasyModeSystem, EasyModeVolume};
    use std::collections::HashMap;

    fn catalogue() -> EasyModeConfig {
        EasyModeConfig {
            system: vec![HashMap::from([(
                "Windows 11".to_string(),
                EasyModeSystem {
                    os_logo: "LOGO_WINDOWS11".to_string(),
                    os_download: "https://example.com/windows11.esd".to_string(),
                    volume: vec![EasyModeVolume {
                        number: 6,
                        name: "专业版".to_string(),
                    }],
                },
            )])],
        }
    }

    #[test]
    fn selecting_system_keeps_first_volume_default() {
        let mut controller = NativeEasyModeController::new(true, false);
        let config = catalogue();
        controller.set_catalogue(Some(&config), false);
        controller.apply(EasyModeAction::SelectSystem(0));
        let view = controller.view();
        assert_eq!(view.selected_volume, Some(0));
        assert_eq!(view.logo, Some(EasyLogoSource::EmbeddedWindows11));
        assert!(view.can_install);
    }

    #[test]
    fn enabled_catalogue_defaults_are_real_controller_selections() {
        let mut controller = NativeEasyModeController::new(true, false);
        let config = catalogue();
        controller.set_catalogue(Some(&config), false);
        let view = controller.view();
        assert_eq!(view.selected_system, Some(0));
        assert_eq!(view.selected_volume, Some(0));
        assert!(view.can_install);
        assert!(controller
            .start_install_intent(Some(0), Path::new("downloads"), None)
            .is_ok());
    }

    #[test]
    fn enabling_after_catalogue_load_selects_the_first_installable_volume() {
        let mut controller = NativeEasyModeController::new(false, false);
        let config = catalogue();
        controller.set_catalogue(Some(&config), false);
        assert!(!controller.view().can_install);
        controller.apply(EasyModeAction::SetEnabled(true));
        assert!(controller.view().can_install);
    }

    #[test]
    fn install_intent_preserves_legacy_defaults_and_route() {
        let mut controller = NativeEasyModeController::new(true, true);
        let config = catalogue();
        controller.set_catalogue(Some(&config), false);
        controller.apply(EasyModeAction::SelectSystem(0));
        let intent = controller
            .start_install_intent(Some(2), Path::new("D:/downloads"), Some("Alice"))
            .unwrap();
        assert_eq!(intent.volume_number, 6);
        assert_eq!(intent.filename, "windows11.esd");
        assert_eq!(intent.system_partition_index, 2);
        assert!(intent.download_then_install && intent.easy_mode_auto_install);
        assert!(intent.prefs.format_partition);
        assert!(intent.prefs.repair_boot);
        assert!(intent.prefs.unattended_install);
        assert!(intent.prefs.auto_reboot);
        assert_eq!(intent.prefs.driver_action, DriverAction::AutoImport);
        assert!(intent.prefs.advanced_options.bypass_nro);
        assert!(intent.prefs.advanced_options.remove_uwp_apps);
        assert!(
            intent
                .prefs
                .advanced_options
                .import_storage_controller_drivers
        );
        assert_eq!(intent.prefs.advanced_options.volume_label, "OS");
        assert_eq!(intent.prefs.advanced_options.username, "Alice");
    }

    #[test]
    fn missing_catalogue_and_partition_fail_without_side_effects() {
        let controller = NativeEasyModeController::new(true, false);
        assert!(matches!(
            controller.start_install_intent(None, Path::new("downloads"), None),
            Err(EasyModeValidationError::CatalogueUnavailable)
        ));
        let mut controller = NativeEasyModeController::new(true, false);
        let config = catalogue();
        controller.set_catalogue(Some(&config), false);
        controller.apply(EasyModeAction::SelectSystem(0));
        assert!(matches!(
            controller.start_install_intent(None, Path::new("downloads"), None),
            Err(EasyModeValidationError::MissingSystemPartition)
        ));
    }
}
