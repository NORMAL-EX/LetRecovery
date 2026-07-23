//! Side-effect-free controller for the native online-download page.
//!
//! The Win32 page owns controls and notifications. This module owns catalogue
//! selection and turns an explicit user action into a validated task plan. It
//! never creates directories, starts aria2, opens files, or performs network
//! requests.

use std::fmt;
use std::path::{Path, PathBuf};

use lr_core::download_integrity::{
    select_expected_hash, validate_download_filename, validate_download_url, DownloadFilenameError,
    DownloadUrlError, IntegrityConfigError, IntegrityRequirement,
};

use crate::download::config::{ConfigManager, OnlineGpuDriver, OnlineSoftware, OnlineSystem};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResourceCategory {
    #[default]
    SystemImage,
    Software,
    GpuDriver,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CatalogueState {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoftwareArchitecture {
    #[default]
    X64,
    X86,
    Nt5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadAction {
    Download,
    InstallAfterDownload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadCompletion {
    None,
    OpenSystemImage(PathBuf),
    RunDownloadedFile(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadPlan {
    pub url: String,
    pub save_directory: PathBuf,
    pub filename: String,
    pub integrity: IntegrityRequirement,
    pub completion: DownloadCompletion,
    /// Per-file parallel connections passed explicitly to aria2.
    pub download_threads: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRow {
    pub name: String,
    pub resource_type: String,
    pub size: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerIntent {
    SelectCategory(ResourceCategory),
    SelectResource(usize),
    RefreshCatalogue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerEffect {
    SelectionChanged,
    RefreshRequested,
}

#[derive(Debug)]
pub enum DownloadPlanError {
    CatalogueUnavailable,
    NoSelection,
    SelectionOutOfRange,
    EmptySaveDirectory,
    InvalidUrl(DownloadUrlError),
    InvalidFilename(DownloadFilenameError),
    InvalidIntegrity(IntegrityConfigError),
}

impl fmt::Display for DownloadPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatalogueUnavailable => f.write_str("online catalogue is not ready"),
            Self::NoSelection => f.write_str("no online resource is selected"),
            Self::SelectionOutOfRange => f.write_str("selected online resource no longer exists"),
            Self::EmptySaveDirectory => f.write_str("download save directory is empty"),
            Self::InvalidUrl(error) => write!(f, "{error}"),
            Self::InvalidFilename(error) => write!(f, "{error}"),
            Self::InvalidIntegrity(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DownloadPlanError {}

#[derive(Clone, Debug, Default)]
pub struct NativeDownloadController {
    state: CatalogueState,
    /// Only the catalogue fetched through LetRecovery's fixed HTTPS endpoint
    /// may opt its selected legacy entries into HTTP transport.  This is kept
    /// separate from the user's explicit compatibility switch so arbitrary or
    /// locally constructed catalogues remain HTTPS-only by default.
    trusted_remote_legacy_http: bool,
    category: ResourceCategory,
    selected_system: Option<usize>,
    selected_software: Option<usize>,
    selected_gpu_driver: Option<usize>,
    systems: Vec<OnlineSystem>,
    software: Vec<OnlineSoftware>,
    gpu_drivers: Vec<OnlineGpuDriver>,
}

impl NativeDownloadController {
    pub fn state(&self) -> &CatalogueState {
        &self.state
    }

    pub const fn category(&self) -> ResourceCategory {
        self.category
    }

    pub fn begin_refresh(&mut self) {
        self.state = CatalogueState::Loading;
    }

    pub fn fail_refresh(&mut self, message: impl Into<String>) {
        self.state = CatalogueState::Failed(message.into());
    }

    /// Replaces the visible catalogue after the existing remote loader has
    /// completed. Network access remains outside this controller.
    pub fn replace_catalogue(&mut self, config: &ConfigManager) {
        self.replace_catalogue_inner(config, false);
    }

    /// Replaces the catalogue obtained from LetRecovery's fixed HTTPS service.
    ///
    /// The service still publishes historical HTTP URLs for some Microsoft
    /// images and third-party software.  Only URLs selected verbatim from this
    /// HTTPS-delivered catalogue receive that compatibility exception; other
    /// catalogues and arbitrary URLs remain subject to the explicit opt-in.
    pub(crate) fn replace_trusted_remote_catalogue(&mut self, config: &ConfigManager) {
        self.replace_catalogue_inner(config, true);
    }

    fn replace_catalogue_inner(&mut self, config: &ConfigManager, trusted_remote: bool) {
        self.systems.clone_from(&config.systems);
        self.software.clone_from(&config.software_list);
        self.gpu_drivers.clone_from(&config.gpu_driver_list);
        self.trusted_remote_legacy_http = trusted_remote;
        self.clamp_selections();
        self.state = CatalogueState::Ready;
    }

    pub fn apply_intent(&mut self, intent: ControllerIntent) -> ControllerEffect {
        match intent {
            ControllerIntent::SelectCategory(category) => {
                self.category = category;
                ControllerEffect::SelectionChanged
            }
            ControllerIntent::SelectResource(index) => {
                *self.active_selection_mut() = Some(index);
                ControllerEffect::SelectionChanged
            }
            ControllerIntent::RefreshCatalogue => {
                self.begin_refresh();
                ControllerEffect::RefreshRequested
            }
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        match self.category {
            ResourceCategory::SystemImage => self.selected_system,
            ResourceCategory::Software => self.selected_software,
            ResourceCategory::GpuDriver => self.selected_gpu_driver,
        }
    }

    pub fn rows(&self) -> Vec<ResourceRow> {
        match self.category {
            ResourceCategory::SystemImage => self
                .systems
                .iter()
                .map(|system| ResourceRow {
                    name: system.display_name.clone(),
                    resource_type: if system.is_win11 { "Win11" } else { "Win10" }.into(),
                    size: String::new(),
                })
                .collect(),
            ResourceCategory::Software => self
                .software
                .iter()
                .map(|software| ResourceRow {
                    name: software.name.clone(),
                    resource_type: "Software".into(),
                    size: software.file_size.clone(),
                })
                .collect(),
            ResourceCategory::GpuDriver => self
                .gpu_drivers
                .iter()
                .map(|driver| ResourceRow {
                    name: driver.name.clone(),
                    resource_type: "GPU driver".into(),
                    size: driver.file_size.clone(),
                })
                .collect(),
        }
    }

    pub fn plan_selected(
        &self,
        action: DownloadAction,
        save_directory: impl AsRef<Path>,
        architecture: SoftwareArchitecture,
        allow_insecure_http: bool,
        download_threads: u8,
    ) -> Result<DownloadPlan, DownloadPlanError> {
        if self.state != CatalogueState::Ready {
            return Err(DownloadPlanError::CatalogueUnavailable);
        }
        let save_directory = save_directory.as_ref();
        if save_directory.as_os_str().is_empty() {
            return Err(DownloadPlanError::EmptySaveDirectory);
        }
        let index = self
            .selected_index()
            .ok_or(DownloadPlanError::NoSelection)?;

        let allow_catalogue_http = allow_insecure_http || self.trusted_remote_legacy_http;
        let (url, filename, completion_kind, sha256, md5) = match self.category {
            ResourceCategory::SystemImage => {
                let system = self
                    .systems
                    .get(index)
                    .ok_or(DownloadPlanError::SelectionOutOfRange)?;
                let validated = validate_download_url(&system.download_url, allow_catalogue_http)
                    .map_err(DownloadPlanError::InvalidUrl)?;
                let filename = match system.filename.as_deref().filter(|value| !value.is_empty()) {
                    Some(filename) => {
                        validate_download_filename(filename)
                            .map_err(DownloadPlanError::InvalidFilename)?;
                        filename.to_string()
                    }
                    None => filename_from_url(validated.as_str(), "system.iso")?,
                };
                (
                    validated.into_string(),
                    filename,
                    CompletionKind::SystemImage,
                    system.sha256.clone(),
                    system.md5.clone(),
                )
            }
            ResourceCategory::Software => {
                let software = self
                    .software
                    .get(index)
                    .ok_or(DownloadPlanError::SelectionOutOfRange)?;
                let source = select_software_source(software, architecture);
                let url = source.url;
                let validated = validate_download_url(url, allow_catalogue_http)
                    .map_err(DownloadPlanError::InvalidUrl)?;
                validate_download_filename(&software.filename)
                    .map_err(DownloadPlanError::InvalidFilename)?;
                (
                    validated.into_string(),
                    software.filename.clone(),
                    CompletionKind::Executable,
                    source.sha256.map(str::to_owned),
                    source.md5.map(str::to_owned),
                )
            }
            ResourceCategory::GpuDriver => {
                let driver = self
                    .gpu_drivers
                    .get(index)
                    .ok_or(DownloadPlanError::SelectionOutOfRange)?;
                let validated = validate_download_url(&driver.download_url, allow_catalogue_http)
                    .map_err(DownloadPlanError::InvalidUrl)?;
                validate_download_filename(&driver.filename)
                    .map_err(DownloadPlanError::InvalidFilename)?;
                (
                    validated.into_string(),
                    driver.filename.clone(),
                    CompletionKind::Executable,
                    driver.sha256.clone(),
                    driver.md5.clone(),
                )
            }
        };

        // Hashes remain optional for compatibility with the current public catalogue.  When the
        // fixed service starts publishing them, they must not be discarded on the way to the
        // executor; a malformed declared hash is still a fail-closed configuration error.
        let integrity = select_expected_hash(sha256.as_deref(), md5.as_deref())
            .map_err(DownloadPlanError::InvalidIntegrity)?;
        let downloaded_path = save_directory.join(&filename);
        let completion = match (action, completion_kind) {
            (DownloadAction::Download, _) => DownloadCompletion::None,
            (DownloadAction::InstallAfterDownload, CompletionKind::SystemImage) => {
                DownloadCompletion::OpenSystemImage(downloaded_path)
            }
            (DownloadAction::InstallAfterDownload, CompletionKind::Executable) => {
                DownloadCompletion::RunDownloadedFile(downloaded_path)
            }
        };

        Ok(DownloadPlan {
            url,
            save_directory: save_directory.to_path_buf(),
            filename,
            integrity,
            completion,
            download_threads: crate::core::app_config::normalize_download_threads(download_threads),
        })
    }

    fn active_selection_mut(&mut self) -> &mut Option<usize> {
        match self.category {
            ResourceCategory::SystemImage => &mut self.selected_system,
            ResourceCategory::Software => &mut self.selected_software,
            ResourceCategory::GpuDriver => &mut self.selected_gpu_driver,
        }
    }

    fn clamp_selections(&mut self) {
        if self
            .selected_system
            .is_some_and(|i| i >= self.systems.len())
        {
            self.selected_system = None;
        }
        if self
            .selected_software
            .is_some_and(|i| i >= self.software.len())
        {
            self.selected_software = None;
        }
        if self
            .selected_gpu_driver
            .is_some_and(|i| i >= self.gpu_drivers.len())
        {
            self.selected_gpu_driver = None;
        }
    }
}

#[derive(Clone, Copy)]
enum CompletionKind {
    SystemImage,
    Executable,
}

struct SoftwareSource<'a> {
    url: &'a str,
    sha256: Option<&'a str>,
    md5: Option<&'a str>,
}

fn select_software_source(
    software: &OnlineSoftware,
    architecture: SoftwareArchitecture,
) -> SoftwareSource<'_> {
    match architecture {
        SoftwareArchitecture::X64 => SoftwareSource {
            url: &software.download_url,
            sha256: software.sha256.as_deref(),
            md5: software.md5.as_deref(),
        },
        SoftwareArchitecture::X86 => match software.download_url_x86.as_deref() {
            Some(url) => SoftwareSource {
                url,
                sha256: software.sha256_x86.as_deref(),
                md5: software.md5_x86.as_deref(),
            },
            None => SoftwareSource {
                url: &software.download_url,
                sha256: software.sha256.as_deref(),
                md5: software.md5.as_deref(),
            },
        },
        SoftwareArchitecture::Nt5 => {
            if let Some(url) = software.download_url_nt5.as_deref() {
                SoftwareSource {
                    url,
                    sha256: software.sha256_nt5.as_deref(),
                    md5: software.md5_nt5.as_deref(),
                }
            } else if let Some(url) = software.download_url_x86.as_deref() {
                SoftwareSource {
                    url,
                    sha256: software.sha256_x86.as_deref(),
                    md5: software.md5_x86.as_deref(),
                }
            } else {
                SoftwareSource {
                    url: &software.download_url,
                    sha256: software.sha256.as_deref(),
                    md5: software.md5.as_deref(),
                }
            }
        }
    }
}

fn filename_from_url(url: &str, fallback: &str) -> Result<String, DownloadPlanError> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let filename = path
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string();
    validate_download_filename(&filename).map_err(DownloadPlanError::InvalidFilename)?;
    Ok(filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ConfigManager {
        ConfigManager {
            systems: vec![OnlineSystem {
                download_url: "https://example.com/Windows11.iso".into(),
                display_name: "Windows 11".into(),
                is_win11: true,
                filename: None,
                md5: None,
                sha256: None,
            }],
            software_list: vec![OnlineSoftware {
                name: "Tool".into(),
                description: String::new(),
                update_date: String::new(),
                file_size: "1 MB".into(),
                icon_url: None,
                download_url: "https://example.com/tool-x64.exe".into(),
                download_url_x86: Some("https://example.com/tool-x86.exe".into()),
                download_url_nt5: Some("https://example.com/tool-nt5.exe".into()),
                filename: "tool.exe".into(),
                md5: None,
                sha256: None,
                md5_x86: None,
                sha256_x86: None,
                md5_nt5: None,
                sha256_nt5: None,
            }],
            gpu_driver_list: vec![OnlineGpuDriver {
                name: "GPU".into(),
                description: String::new(),
                update_date: String::new(),
                file_size: "2 MB".into(),
                icon_url: None,
                download_url: "https://example.com/gpu.exe".into(),
                filename: "gpu.exe".into(),
                md5: None,
                sha256: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn system_install_plan_is_validated_and_points_to_downloaded_image() {
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config());
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let plan = controller
            .plan_selected(
                DownloadAction::InstallAfterDownload,
                r"D:\Downloads",
                SoftwareArchitecture::X64,
                false,
                16,
            )
            .unwrap();

        assert_eq!(plan.filename, "Windows11.iso");
        assert_eq!(plan.integrity, IntegrityRequirement::NotProvided);
        assert_eq!(
            plan.completion,
            DownloadCompletion::OpenSystemImage(PathBuf::from(r"D:\Downloads\Windows11.iso"))
        );
    }

    #[test]
    fn declared_system_sha256_reaches_the_download_executor() {
        let mut config = config();
        config.systems[0].filename = Some("server-name.esd".into());
        config.systems[0].sha256 =
            Some("BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD".into());
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config);
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let plan = controller
            .plan_selected(
                DownloadAction::Download,
                r"D:\Downloads",
                SoftwareArchitecture::X64,
                false,
                16,
            )
            .unwrap();

        assert_eq!(plan.filename, "server-name.esd");
        assert!(matches!(plan.integrity, IntegrityRequirement::Required(_)));
    }

    #[test]
    fn alternate_architecture_never_reuses_the_x64_hash() {
        let mut config = config();
        config.software_list[0].sha256 =
            Some("BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD".into());
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config);
        controller.apply_intent(ControllerIntent::SelectCategory(ResourceCategory::Software));
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let plan = controller
            .plan_selected(
                DownloadAction::Download,
                r"D:\Downloads",
                SoftwareArchitecture::X86,
                false,
                16,
            )
            .unwrap();

        assert_eq!(plan.integrity, IntegrityRequirement::NotProvided);
    }

    #[test]
    fn software_architecture_urls_preserve_nt5_and_x86_fallbacks() {
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config());
        controller.apply_intent(ControllerIntent::SelectCategory(ResourceCategory::Software));
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let plan = controller
            .plan_selected(
                DownloadAction::Download,
                r"D:\Downloads",
                SoftwareArchitecture::Nt5,
                false,
                16,
            )
            .unwrap();
        assert_eq!(plan.url, "https://example.com/tool-nt5.exe");
        assert_eq!(plan.completion, DownloadCompletion::None);
    }

    #[test]
    fn http_requires_explicit_compatibility_opt_in() {
        let mut config = config();
        config.systems[0].download_url = "http://example.com/system.iso".into();
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config);
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let error = controller
            .plan_selected(
                DownloadAction::Download,
                r"D:\Downloads",
                SoftwareArchitecture::X64,
                false,
                16,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DownloadPlanError::InvalidUrl(DownloadUrlError::HttpRequiresOptIn)
        ));
    }

    #[test]
    fn fixed_https_catalogue_may_preserve_its_selected_legacy_http_url() {
        let mut config = config();
        config.software_list[0].download_url =
            "http://pan.yyej.com/f/example/legacy-tool.exe".into();
        let mut controller = NativeDownloadController::default();
        controller.replace_trusted_remote_catalogue(&config);
        controller.apply_intent(ControllerIntent::SelectCategory(ResourceCategory::Software));
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let plan = controller
            .plan_selected(
                DownloadAction::InstallAfterDownload,
                r"D:\Downloads",
                SoftwareArchitecture::X64,
                false,
                16,
            )
            .unwrap();

        assert_eq!(plan.url, "http://pan.yyej.com/f/example/legacy-tool.exe");
        assert_eq!(
            plan.completion,
            DownloadCompletion::RunDownloadedFile(PathBuf::from(r"D:\Downloads\tool.exe"))
        );
    }

    #[test]
    fn replacing_trusted_catalogue_with_local_data_revokes_http_compatibility() {
        let mut config = config();
        config.systems[0].download_url = "http://example.com/system.iso".into();
        let mut controller = NativeDownloadController::default();
        controller.replace_trusted_remote_catalogue(&config);
        controller.replace_catalogue(&config);
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let error = controller
            .plan_selected(
                DownloadAction::Download,
                r"D:\Downloads",
                SoftwareArchitecture::X64,
                false,
                16,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DownloadPlanError::InvalidUrl(DownloadUrlError::HttpRequiresOptIn)
        ));
    }

    #[test]
    fn server_filename_cannot_escape_save_directory() {
        let mut config = config();
        config.software_list[0].filename = r"..\evil.exe".into();
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config);
        controller.apply_intent(ControllerIntent::SelectCategory(ResourceCategory::Software));
        controller.apply_intent(ControllerIntent::SelectResource(0));

        let error = controller
            .plan_selected(
                DownloadAction::Download,
                r"D:\Downloads",
                SoftwareArchitecture::X64,
                false,
                16,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DownloadPlanError::InvalidFilename(DownloadFilenameError::PathComponent)
        ));
    }

    #[test]
    fn selection_is_cleared_when_refreshed_catalogue_shrinks() {
        let mut controller = NativeDownloadController::default();
        controller.replace_catalogue(&config());
        controller.apply_intent(ControllerIntent::SelectResource(8));
        controller.replace_catalogue(&config());
        assert_eq!(controller.selected_index(), None);
    }
}
