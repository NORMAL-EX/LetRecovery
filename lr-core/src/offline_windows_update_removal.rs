//! Offline removal of the active Windows Update surface for Windows 7 through Windows 11.
//!
//! This intentionally preserves CBS package metadata and every WinSxS component-store payload.
//! The active System32/SysWOW64/UUS names are removed one pathname at a time (files may be
//! legitimate hard links to WinSxS), while UsoSvc/WaaSMedicSvc, their scheduled-task cache,
//! stable runtime registrations, and Settings pages are removed or hidden with strict readback.
//! PolicyDefinitions ADMX/ADML files are deliberately preserved: they are passive Group Policy
//! editor schemas (and may be servicing-store hard links), not active Windows Update components.
//! Windows NT 6.x uses the legacy WUA profile; Windows 10/11 use the USO profile. The profiles were
//! derived from disposable-image NTLite oracles for 7601, 19045, 22635, 26100, and 28000, then
//! expressed as an exact union of known component names. A build outside the Windows 7--11 client
//! ranges fails before the first mutation instead of receiving a guessed profile.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::registry::OfflineRegistry;
use crate::scoped_temp_file::{pin_existing_directory_ancestors, PinnedDirectoryAncestors};

const SETTINGS_POLICY_KEY_SUFFIX: &str = "Microsoft\\Windows\\CurrentVersion\\Policies\\Explorer";
const SETTINGS_POLICY_VALUE: &str = "SettingsPageVisibility";
const UPDATE_SETTINGS_PAGES: [&str; 6] = [
    "installedupdates",
    "windowsupdate",
    "windowsupdate-action",
    "windowsupdate-history",
    "windowsupdate-options",
    "windowsupdate-restartoptions",
];

const MODERN_ACTIVE_PATHS: &[&str] = &[
    "ProgramData\\Microsoft\\Windows\\Models",
    "ProgramData\\Microsoft\\Windows\\UUS",
    "ProgramData\\SoftwareDistribution",
    "ProgramData\\USOPrivate",
    "ProgramData\\USOShared",
    "Windows\\diagnostics\\index\\WindowsUpdateDiagnostic.xml",
    "Windows\\diagnostics\\system\\WindowsUpdate",
    "Windows\\System32\\@WindowsUpdateToastIcon.contrast-black.png",
    "Windows\\System32\\@WindowsUpdateToastIcon.contrast-white.png",
    "Windows\\System32\\@WindowsUpdateToastIcon.png",
    "Windows\\System32\\ActiveHours.png",
    "Windows\\System32\\DeviceFeatureDDF.json",
    "Windows\\System32\\Facilitator.dll",
    "Windows\\System32\\MoUsoCoreWorker.exe",
    "Windows\\System32\\musdialoghandlers.dll",
    "Windows\\System32\\MusNotification.exe",
    "Windows\\System32\\MusNotificationUx.exe",
    "Windows\\System32\\MusNotifyIcon.exe",
    "Windows\\System32\\MLEngineStub.exe",
    "Windows\\System32\\MoNotificationUxStub.exe",
    "Windows\\System32\\MusAppUpdateHandlers.dll",
    "Windows\\System32\\museuxdocked.dll",
    "Windows\\System32\\MusUpdateHandlers.dll",
    "Windows\\System32\\MusUpdateHandlers1.dll",
    "Windows\\System32\\SettingsHandlers_InstalledUpdates.dll",
    "Windows\\System32\\UdiApiClient.dll",
    "Windows\\System32\\UIEApi.dll",
    "Windows\\System32\\UIEOrchestrator.exe",
    "Windows\\System32\\UIEOrchestratorStub.exe",
    "Windows\\System32\\UpdateAgent.dll",
    "Windows\\System32\\updatecli.exe",
    "Windows\\System32\\upfc.exe",
    "Windows\\System32\\upshared.dll",
    "Windows\\System32\\usoapi.dll",
    "Windows\\System32\\UsoClient.exe",
    "Windows\\System32\\usocoreps.dll",
    "Windows\\System32\\usocoreworker.exe",
    "Windows\\System32\\usodocked.dll",
    "Windows\\System32\\usosvc.dll",
    "Windows\\System32\\usosvcimpl.dll",
    "Windows\\System32\\WaaSAssessment.dll",
    "Windows\\System32\\WaaSMedicAgent.exe",
    "Windows\\System32\\WaaSMedicCapsule.dll",
    "Windows\\System32\\WaaSMedicPS.dll",
    "Windows\\System32\\WaaSMedicSvc.dll",
    "Windows\\System32\\Windows.Internal.WaaSMedicDocked.dll",
    "Windows\\System32\\Windows.Management.Update.dll",
    "Windows\\System32\\WindowsPowerShell\\v1.0\\Modules\\WindowsUpdate",
    "Windows\\System32\\WindowsUpdateElevatedInstaller.exe",
    "Windows\\System32\\wudriver.dll",
    "Windows\\System32\\wusa.exe",
    "Windows\\System32\\wuuhosdeployment.dll",
    "Windows\\SysWOW64\\usoapi.dll",
    "Windows\\SysWOW64\\Windows.Management.Update.dll",
    "Windows\\SysWOW64\\WindowsPowerShell\\v1.0\\Modules\\WindowsUpdate",
    "Windows\\SysWOW64\\wuapi.dll",
    "Windows\\SysWOW64\\wudriver.dll",
    "Windows\\SysWOW64\\wups.dll",
    "Windows\\SysWOW64\\wusa.exe",
    "Windows\\SysWOW64\\wusys.dll",
    "Windows\\UUS",
    "Windows\\WaaS\\regkeys",
    "Windows\\WaaS\\services",
    "Windows\\WaaS\\tasks",
    "Windows\\WUModels",
];

const MODERN_REQUIRED_ANCHOR_PATHS: &[&str] = &[
    "Windows\\System32\\UsoClient.exe",
    "Windows\\System32\\usosvc.dll",
    "Windows\\UUS",
];

// NTLite 2026.06.10598, Windows 7 Ultimate SP1 x64 6.1.7601.17514, component
// `windowsupdate 'Windows 更新'`. WinSxS and its backup/cache entries are deliberately excluded:
// those are servicing metadata, not active names, and removing them would make recovery harder.
const LEGACY_ACTIVE_PATHS: &[&str] = &[
    "ProgramData\\Microsoft\\Windows\\Start Menu\\Windows Update.lnk",
    "Windows\\diagnostics\\index\\WindowsUpdateDiagnostic.xml",
    "Windows\\diagnostics\\system\\WindowsUpdate",
    "Windows\\System32\\chkwudrv.dll",
    "Windows\\System32\\wuapp.exe",
    "Windows\\System32\\wuauclt.exe",
    "Windows\\System32\\wuaueng.dll",
    "Windows\\System32\\wucltux.dll",
    "Windows\\System32\\wudriver.dll",
    "Windows\\System32\\wups.dll",
    "Windows\\System32\\wups2.dll",
    "Windows\\System32\\wusa.exe",
    "Windows\\System32\\wuwebv.dll",
    "Windows\\SysWOW64\\wuapi.dll",
    "Windows\\SysWOW64\\wuapp.exe",
    "Windows\\SysWOW64\\wudriver.dll",
    "Windows\\SysWOW64\\wups.dll",
    "Windows\\SysWOW64\\wusa.exe",
    "Windows\\SysWOW64\\wuwebv.dll",
];

const LEGACY_SYSTEM32_LOCALIZED_NAMES: &[&str] = &[
    "chkwudrv.dll.mui",
    "wuapi.dll.mui",
    "wuaueng.dll.mui",
    "wucltux.dll.mui",
    "wusa.exe.mui",
];
const LEGACY_SYSWOW64_LOCALIZED_NAMES: &[&str] = &["wuapi.dll.mui", "wusa.exe.mui"];
const MODERN_SYSTEM32_LOCALIZED_NAMES: &[&str] = &[
    "MusAppUpdateHandlers.dll.mui",
    "MusUpdateHandlers.dll.mui",
    "MusUpdateHandlers1.dll.mui",
    "SettingsHandlers_InstalledUpdates.dll.mui",
    "usosvc.dll.mui",
    "wusa.exe.mui",
];
const MODERN_SYSWOW64_LOCALIZED_NAMES: &[&str] = &["wusa.exe.mui"];

const LEGACY_REQUIRED_ANCHOR_PATHS: &[&str] = &[
    "Windows\\System32\\wuapp.exe",
    "Windows\\System32\\wuaueng.dll",
];

const LEGACY_SERVICES: &[&str] = &["wuauserv"];
const MODERN_SERVICES: &[&str] = &["UsoSvc", "WaaSMedicSvc"];

const MODERN_UPDATE_TASKS: &[&str] = &[
    "Microsoft\\Windows\\UpdateOrchestrator\\Report policies",
    "Microsoft\\Windows\\UpdateOrchestrator\\Schedule Scan Static Task",
    "Microsoft\\Windows\\UpdateOrchestrator\\UIEOrchestrator",
    "Microsoft\\Windows\\UpdateOrchestrator\\UpdateModelTask",
    "Microsoft\\Windows\\UpdateOrchestrator\\USO_UxBroker",
    "Microsoft\\Windows\\UpdateOrchestrator\\UUS Failover Task",
    "Microsoft\\Windows\\WaaSMedic\\PerformRemediation",
    "Microsoft\\Windows\\WindowsUpdate\\Refresh Group Policy Cache",
];

const MODERN_SOFTWARE_KEYS: &[&str] = &[
    "Classes\\AppID\\mousocoreworker.exe",
    "Classes\\AppID\\usocoreworker.exe",
    "Classes\\AppUserModelId\\Windows.SystemToast.WindowsUpdate.MoNotification",
    "Classes\\AppUserModelId\\Windows.SystemToast.WindowsUpdate.MoNotification2",
    "Classes\\AppUserModelId\\Windows.SystemToast.WindowsUpdate.Notification",
    "Classes\\AppUserModelId\\Windows.SystemToast.WindowsUpdate.MoNotificationApps",
    "Classes\\Microsoft.WaaSMedic",
    "Classes\\Microsoft.WaaSMedic.1",
    "Microsoft\\SystemSettings\\SettingId\\SystemSettings_MusUpdate_AllowAutoWindowsUpdateDownloadOverMeteredNetwork",
    "Microsoft\\Windows NT\\CurrentVersion\\Update\\TargetingInfo\\DynamicInstalled\\UUS.amd64",
    "Microsoft\\Windows\\CurrentVersion\\AppModel\\LimitedAccessFeatures\\com.microsoft.windows.updateorchestrator.1",
    "Microsoft\\Windows\\CurrentVersion\\PushNotifications\\Applications\\Windows.SystemToast.WindowsUpdate.MoNotification",
    "Microsoft\\Windows\\CurrentVersion\\PushNotifications\\Applications\\Windows.SystemToast.WindowsUpdate.MoNotification2",
    "Microsoft\\Windows\\CurrentVersion\\PushNotifications\\Applications\\Windows.SystemToast.WindowsUpdate.Notification",
    "Microsoft\\Windows\\CurrentVersion\\SignalManager\\InboxStore\\WindowsUpdateRebootDowntimeEstimate",
    "Microsoft\\Windows\\CurrentVersion\\WaaSAssessment",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Internal.WaaSMedicDocked.CBSHelper",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Internal.WaaSMedicDocked.WaaSAssessmentHelper",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateAdministrator",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateApprovalData",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateManager",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateManagerScanOptions",
    "Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateRestartRequestOptions",
    "Microsoft\\WindowsUpdate\\EditionSettings",
    "Microsoft\\WindowsUpdate\\Orchestrator\\USOShared\\EtwSessionsToDump",
    "Microsoft\\WindowsUpdate\\StandaloneInstaller",
    "Microsoft\\WindowsUpdate\\UIEOrch",
    "Microsoft\\WindowsUpdate\\UpdateHandlers\\OSDeployment",
    "Microsoft\\WindowsUpdate\\UX",
    "WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\AppModel\\LimitedAccessFeatures\\com.microsoft.windows.updateorchestrator.1",
    "WOW6432Node\\Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateAdministrator",
    "WOW6432Node\\Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateApprovalData",
    "WOW6432Node\\Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateManager",
    "WOW6432Node\\Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateManagerScanOptions",
    "WOW6432Node\\Microsoft\\WindowsRuntime\\ActivatableClassId\\Windows.Management.Update.WindowsUpdateRestartRequestOptions",
    "WOW6432Node\\Microsoft\\WindowsUpdate\\EditionSettings",
    "WOW6432Node\\Microsoft\\WindowsUpdate\\StandaloneInstaller",
    "WOW6432Node\\Microsoft\\WindowsUpdate",
    "Microsoft\\Windows NT\\CurrentVersion\\Schedule\\TaskCache\\Tree\\Microsoft\\Windows\\WaaSMedic",
];

const LEGACY_SOFTWARE_KEYS: &[&str] = &[
    "Microsoft\\Windows\\CurrentVersion\\WINEVT\\Channels\\Microsoft-Windows-WindowsUpdateClient/Operational",
];

const LEGACY_SYSTEM_KEYS: &[&str] =
    &["Services\\eventlog\\System\\Microsoft-Windows-WindowsUpdateClient"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsUpdateProfileKind {
    LegacyWua,
    ModernUso,
}

#[derive(Debug, Clone, Copy)]
struct WindowsUpdateProfile {
    name: &'static str,
    kind: WindowsUpdateProfileKind,
    active_paths: &'static [&'static str],
    required_anchor_paths: &'static [&'static str],
    services: &'static [&'static str],
    tasks: &'static [&'static str],
    software_keys: &'static [&'static str],
    system_keys: &'static [&'static str],
    hide_settings_pages: bool,
}

const LEGACY_PROFILE: WindowsUpdateProfile = WindowsUpdateProfile {
    name: "windows-nt6-legacy-wua",
    kind: WindowsUpdateProfileKind::LegacyWua,
    active_paths: LEGACY_ACTIVE_PATHS,
    required_anchor_paths: LEGACY_REQUIRED_ANCHOR_PATHS,
    services: LEGACY_SERVICES,
    tasks: &[],
    software_keys: LEGACY_SOFTWARE_KEYS,
    system_keys: LEGACY_SYSTEM_KEYS,
    hide_settings_pages: false,
};

const MODERN_PROFILE: WindowsUpdateProfile = WindowsUpdateProfile {
    name: "windows-10-11-uso",
    kind: WindowsUpdateProfileKind::ModernUso,
    active_paths: MODERN_ACTIVE_PATHS,
    required_anchor_paths: MODERN_REQUIRED_ANCHOR_PATHS,
    services: MODERN_SERVICES,
    tasks: MODERN_UPDATE_TASKS,
    software_keys: MODERN_SOFTWARE_KEYS,
    system_keys: &[],
    hide_settings_pages: true,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsUpdateRemovalReport {
    pub profile: &'static str,
    pub target_build: u32,
    pub removed_paths: usize,
    pub removed_services: usize,
    pub removed_task_trees: usize,
    pub removed_task_records: usize,
    pub removed_registry_keys: usize,
    pub deleted_ubpm_values: usize,
    pub settings_page_visibility: Option<String>,
    pub warnings: Vec<String>,
}

fn record_warning(warnings: &mut Vec<String>, message: String) {
    const MAX_WARNINGS: usize = 32;
    if warnings.len() < MAX_WARNINGS {
        warnings.push(message);
    }
}

#[derive(Debug)]
struct VerifiedTarget {
    root: PathBuf,
    pins: PinnedDirectoryAncestors,
}

impl VerifiedTarget {
    fn verify(&self) -> Result<()> {
        self.pins
            .verify_unchanged()
            .context("offline Windows target ancestor identity changed")
    }
}

fn validate_alias(alias: &str) -> Result<()> {
    if alias.is_empty()
        || !alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("invalid offline registry hive alias: {alias:?}");
    }
    Ok(())
}

fn verified_target(target_partition: &str) -> Result<VerifiedTarget> {
    let value = target_partition.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        anyhow::bail!("target partition must be a drive letter, got {target_partition:?}");
    }
    let root = PathBuf::from(format!("{}\\", value.to_ascii_uppercase()));
    let config = root.join("Windows\\System32\\config");
    let pins = pin_existing_directory_ancestors(&config)
        .with_context(|| format!("pin offline Windows target below {}", root.display()))?;
    pins.verify_unchanged()?;
    for hive in ["SOFTWARE", "SYSTEM"] {
        let path = config.join(hive);
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("inspect offline {hive} hive {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            anyhow::bail!(
                "offline {hive} hive is not an ordinary file: {}",
                path.display()
            );
        }
    }
    Ok(VerifiedTarget { root, pins })
}

fn discover_control_sets(system_hive_alias: &str) -> Result<Vec<u32>> {
    let select_key = format!("HKLM\\{}\\Select", system_hive_alias);
    let mut values = BTreeSet::new();
    for name in ["Current", "Default", "LastKnownGood"] {
        if let Some(value) = OfflineRegistry::query_dword_optional(&select_key, name)? {
            if (1..=999).contains(&value) {
                values.insert(value);
            }
        }
    }
    if values.is_empty() {
        anyhow::bail!("offline SYSTEM hive exposes no valid active control set");
    }
    Ok(values.into_iter().collect())
}

fn is_braced_guid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && bytes[1..37].iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn merge_settings_page_visibility(existing: Option<&str>) -> Result<String> {
    let required = UPDATE_SETTINGS_PAGES;
    match existing.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(format!("hide:{}", required.join(";"))),
        Some(value) if value.len() >= 5 && value[..5].eq_ignore_ascii_case("hide:") => {
            let mut pages = value[5..]
                .split(';')
                .map(str::trim)
                .filter(|page| !page.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for page in required {
                if !pages.iter().any(|item| item.eq_ignore_ascii_case(page)) {
                    pages.push(page.to_owned());
                }
            }
            Ok(format!("hide:{}", pages.join(";")))
        }
        Some(value) if value.len() >= 9 && value[..9].eq_ignore_ascii_case("showonly:") => {
            let pages = value[9..]
                .split(';')
                .map(str::trim)
                .filter(|page| !page.is_empty())
                .filter(|page| !required.iter().any(|item| item.eq_ignore_ascii_case(page)))
                .collect::<Vec<_>>();
            Ok(format!("showonly:{}", pages.join(";")))
        }
        Some(value) => anyhow::bail!(
            "unsupported existing SettingsPageVisibility syntax; refusing to overwrite: {value:?}"
        ),
    }
}

fn visibility_hides_update_pages(value: &str) -> bool {
    let value = value.trim();
    if value.len() >= 5 && value[..5].eq_ignore_ascii_case("hide:") {
        let pages = value[5..].split(';').map(str::trim).collect::<Vec<_>>();
        return UPDATE_SETTINGS_PAGES
            .iter()
            .all(|required| pages.iter().any(|page| page.eq_ignore_ascii_case(required)));
    }
    if value.len() >= 9 && value[..9].eq_ignore_ascii_case("showonly:") {
        let pages = value[9..].split(';').map(str::trim).collect::<Vec<_>>();
        return UPDATE_SETTINGS_PAGES
            .iter()
            .all(|required| !pages.iter().any(|page| page.eq_ignore_ascii_case(required)));
    }
    false
}

fn collect_task_ids(
    software_hive_alias: &str,
    tasks: &[&str],
) -> Result<Vec<(String, Option<String>)>> {
    let base = format!(
        "HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion\\Schedule\\TaskCache\\Tree",
        software_hive_alias
    );
    tasks
        .iter()
        .map(|relative| {
            let key = format!("{}\\{}", base, relative);
            let id = OfflineRegistry::query_string_optional(&key, "Id")?;
            if let Some(id) = &id {
                if !is_braced_guid(id) {
                    anyhow::bail!("invalid scheduled-task cache Id at {key}: {id:?}");
                }
            }
            Ok((key, id))
        })
        .collect()
}

fn parse_build_number(value: &str) -> Result<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        anyhow::bail!("invalid CurrentBuildNumber: {value:?}");
    }
    value
        .parse::<u32>()
        .with_context(|| format!("parse CurrentBuildNumber {value:?}"))
}

fn profile_for_build(build: u32) -> Result<&'static WindowsUpdateProfile> {
    match build {
        // Windows 7, 8, and 8.1 all use the legacy Windows Update Agent service/binary surface.
        // Exact path presence is checked again against the selected image before mutation.
        7_600..=9_600 => Ok(&LEGACY_PROFILE),
        // Windows 10 client builds. 1904x servicing revisions retain the same CurrentBuildNumber.
        10_240..=19_045 => Ok(&MODERN_PROFILE),
        // Windows 11 client families. Disposable oracles cover 22635, 26100, and 28000; exact
        // service and active-path anchors are still required before mutating another build.
        22_000..=28_999 => Ok(&MODERN_PROFILE),
        _ => anyhow::bail!(
            "Windows Update component removal has no audited Windows 7-11 client profile for build {build}"
        ),
    }
}

/// Returns whether an offline client build has a Windows Update removal profile.
pub fn supports_build_number(build: u32) -> bool {
    profile_for_build(build).is_ok()
}

fn is_update_ubpm_value_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("maintenancework")
        || name.contains("schedulewaketowork")
        || name.contains("schedulework")
        || name.contains("univorch")
        || name.contains("uso")
        || name.contains("us03")
        || name.contains("us04")
        || name.contains("wuscheduledstart")
}

fn collect_modern_setting_keys(software_hive_alias: &str) -> Result<Vec<String>> {
    let base = format!(
        "HKLM\\{}\\Microsoft\\SystemSettings\\SettingId",
        software_hive_alias
    );
    if !OfflineRegistry::key_exists(&base)? {
        return Ok(Vec::new());
    }
    let mut keys = OfflineRegistry::enumerate_subkeys(&base)?
        .into_iter()
        .filter(|name| {
            name.to_ascii_lowercase()
                .starts_with("systemsettings_musupdate_")
                || name
                    .to_ascii_lowercase()
                    .starts_with("systemsettings_musappupdate_")
                || name.eq_ignore_ascii_case("SystemSettings_Flights_PauseUpdate")
                || name.eq_ignore_ascii_case("SystemSettings_InstalledUpdates_UpdatesList")
        })
        .map(|name| format!("{base}\\{name}"))
        .collect::<Vec<_>>();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    Ok(keys)
}

fn collect_localized_active_paths(
    target_root: &std::path::Path,
    profile_kind: WindowsUpdateProfileKind,
) -> Result<Vec<PathBuf>> {
    let mut locations: Vec<(&str, &[&str])> = match profile_kind {
        WindowsUpdateProfileKind::LegacyWua => vec![
            ("Windows\\System32", LEGACY_SYSTEM32_LOCALIZED_NAMES),
            ("Windows\\SysWOW64", LEGACY_SYSWOW64_LOCALIZED_NAMES),
        ],
        WindowsUpdateProfileKind::ModernUso => vec![
            ("Windows\\System32", MODERN_SYSTEM32_LOCALIZED_NAMES),
            ("Windows\\SysWOW64", MODERN_SYSWOW64_LOCALIZED_NAMES),
        ],
    };
    let mut paths = Vec::new();
    for (relative_base, file_names) in locations.drain(..) {
        let base = target_root.join(relative_base);
        let base_metadata = match std::fs::symlink_metadata(&base) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect localized-resource base {}", base.display())
                });
            }
        };
        if !base_metadata.is_dir() || base_metadata.file_type().is_symlink() {
            anyhow::bail!(
                "localized-resource base is not an ordinary directory: {}",
                base.display()
            );
        }
        for entry in std::fs::read_dir(&base)
            .with_context(|| format!("enumerate localized-resource base {}", base.display()))?
        {
            let entry = entry.with_context(|| {
                format!("read localized-resource entry below {}", base.display())
            })?;
            let directory = entry.path();
            let metadata = std::fs::symlink_metadata(&directory).with_context(|| {
                format!("inspect localized-resource entry {}", directory.display())
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            for file_name in file_names {
                let candidate = directory.join(file_name);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        paths.push(candidate);
                    }
                    Ok(_) => anyhow::bail!(
                        "localized Windows Update payload is not an ordinary file: {}",
                        candidate.display()
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "inspect localized Windows Update payload {}",
                                candidate.display()
                            )
                        });
                    }
                }
            }
        }
    }
    paths.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
    paths.dedup_by(|left, right| {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    });
    Ok(paths)
}

/// Remove the audited active Windows Update surface from an offline Windows 7--11 installation.
///
/// The caller must already have loaded the target SOFTWARE and SYSTEM hives under the supplied
/// aliases. Build, target, policy syntax, task IDs, required active payloads, and services are all
/// validated before the first write. Missing optional active paths are accepted for language and
/// edition compatibility; every path actually in the plan is read back as absent.
pub fn remove_offline_windows_update(
    target_partition: &str,
    software_hive_alias: &str,
    system_hive_alias: &str,
) -> Result<WindowsUpdateRemovalReport> {
    validate_alias(software_hive_alias)?;
    validate_alias(system_hive_alias)?;
    let target = verified_target(target_partition)?;
    target.verify()?;

    let current_version = format!(
        "HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion",
        software_hive_alias
    );
    let build_text = OfflineRegistry::query_string(&current_version, "CurrentBuildNumber")?;
    let build = parse_build_number(&build_text)?;
    let profile = profile_for_build(build)?;
    let control_sets = discover_control_sets(system_hive_alias)?;
    let service_keys = control_sets
        .iter()
        .flat_map(|control_set| {
            profile.services.iter().map(move |service| {
                format!(
                    "HKLM\\{}\\ControlSet{:03}\\Services\\{}",
                    system_hive_alias, control_set, service
                )
            })
        })
        .collect::<Vec<_>>();
    let mut service_anchor_present = false;
    for key in &service_keys {
        service_anchor_present |= OfflineRegistry::key_exists(key)?;
    }
    if !service_anchor_present {
        anyhow::bail!(
            "Windows Update profile {} has no service anchor before removal",
            profile.name
        );
    }
    let mut file_anchor_present = false;
    for relative in profile.required_anchor_paths {
        let path = target.root.join(relative);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    anyhow::bail!(
                        "Windows Update anchor is a reparse point: {}",
                        path.display()
                    );
                }
                file_anchor_present = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect Windows Update anchor {}", path.display()));
            }
        }
    }
    if !file_anchor_present {
        anyhow::bail!(
            "Windows Update profile {} has no active-file anchor before removal",
            profile.name
        );
    }
    let task_ids = collect_task_ids(software_hive_alias, profile.tasks)?;
    let settings_key = format!(
        "HKLM\\{}\\{}",
        software_hive_alias, SETTINGS_POLICY_KEY_SUFFIX
    );
    let settings_visibility = if profile.hide_settings_pages {
        let existing_visibility =
            OfflineRegistry::query_string_optional(&settings_key, SETTINGS_POLICY_VALUE)?;
        Some(merge_settings_page_visibility(
            existing_visibility.as_deref(),
        )?)
    } else {
        None
    };
    let modern_setting_keys = if profile.kind == WindowsUpdateProfileKind::ModernUso {
        collect_modern_setting_keys(software_hive_alias)?
    } else {
        Vec::new()
    };
    let mut planned_paths = profile
        .active_paths
        .iter()
        .map(|relative| target.root.join(relative))
        .collect::<Vec<_>>();
    planned_paths.extend(collect_localized_active_paths(&target.root, profile.kind)?);
    let mut ubpm_values = Vec::new();
    if profile.kind == WindowsUpdateProfileKind::ModernUso {
        for control_set in &control_sets {
            let key = format!(
                "HKLM\\{}\\ControlSet{:03}\\Control\\Ubpm",
                system_hive_alias, control_set
            );
            if OfflineRegistry::key_exists(&key)? {
                let names = OfflineRegistry::enumerate_value_names(&key)?
                    .into_iter()
                    .filter(|name| is_update_ubpm_value_name(name))
                    .collect::<Vec<_>>();
                ubpm_values.push((key, names));
            }
        }
    }

    // These two policy outcomes are the stable, documented user-visible fallback even when an
    // edition-specific payload cannot be removed. Perform and verify them before the independent
    // batch removals so a single hard-link/ACL failure cannot leave Windows Update fully active.
    let auto_update_key = format!(
        "HKLM\\{}\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
        software_hive_alias
    );
    OfflineRegistry::set_dword(&auto_update_key, "NoAutoUpdate", 1)?;
    if let Some(settings_visibility) = &settings_visibility {
        OfflineRegistry::set_string(&settings_key, SETTINGS_POLICY_VALUE, settings_visibility)?;
    }
    if OfflineRegistry::query_dword(&auto_update_key, "NoAutoUpdate")? != 1 {
        anyhow::bail!("NoAutoUpdate policy readback does not equal 1");
    }
    if let Some(settings_visibility) = &settings_visibility {
        let observed_visibility =
            OfflineRegistry::query_string(&settings_key, SETTINGS_POLICY_VALUE)?;
        if observed_visibility != *settings_visibility
            || !visibility_hides_update_pages(&observed_visibility)
        {
            anyhow::bail!(
                "SettingsPageVisibility readback does not hide every Windows Update page"
            );
        }
    }

    crate::defender_removal::enable_file_removal_privileges()?;
    let mut warnings = Vec::new();
    let mut removed_paths = 0;
    for path in &planned_paths {
        target.verify()?;
        if !path.starts_with(&target.root) {
            anyhow::bail!(
                "Windows Update removal path escaped target root: {}",
                path.display()
            );
        }
        match crate::defender_removal::remove_owned_path_with_file_hard_links(path) {
            Ok(true) => removed_paths += 1,
            Ok(false) => {}
            Err(error) => record_warning(
                &mut warnings,
                format!("remove active path {}: {error:#}", path.display()),
            ),
        }
        target.verify()?;
    }

    let mut removed_task_records = 0;
    let task_cache_base = format!(
        "HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion\\Schedule\\TaskCache",
        software_hive_alias
    );
    for (_, id) in &task_ids {
        if let Some(id) = id {
            for category in ["Tasks", "Plain", "Boot", "Logon", "Maintenance"] {
                let record = format!("{}\\{}\\{}", task_cache_base, category, id);
                match OfflineRegistry::delete_key_verified(&record) {
                    Ok(true) => removed_task_records += 1,
                    Ok(false) => {}
                    Err(error) => record_warning(
                        &mut warnings,
                        format!("delete task record {record}: {error:#}"),
                    ),
                }
            }
        }
    }
    let mut removed_task_trees = 0;
    for (key, _) in &task_ids {
        match OfflineRegistry::delete_key_verified(key) {
            Ok(true) => removed_task_trees += 1,
            Ok(false) => {}
            Err(error) => {
                record_warning(&mut warnings, format!("delete task tree {key}: {error:#}"))
            }
        }
    }

    let mut removed_services = 0;
    for key in &service_keys {
        match OfflineRegistry::delete_key_verified(key) {
            Ok(true) => removed_services += 1,
            Ok(false) => {}
            Err(error) => record_warning(&mut warnings, format!("delete service {key}: {error:#}")),
        }
    }
    let mut deleted_ubpm_values = 0;
    for (key, names) in &ubpm_values {
        for name in names {
            match OfflineRegistry::delete_value(key, name) {
                Ok(()) => match OfflineRegistry::enumerate_value_names(key) {
                    Ok(names) if !names.iter().any(|item| item.eq_ignore_ascii_case(name)) => {
                        deleted_ubpm_values += 1;
                    }
                    Ok(_) => record_warning(
                        &mut warnings,
                        format!("Windows Update UBPM value survived removal: {key}\\{name}"),
                    ),
                    Err(error) => record_warning(
                        &mut warnings,
                        format!("read back UBPM value {key}\\{name}: {error:#}"),
                    ),
                },
                Err(error) => record_warning(
                    &mut warnings,
                    format!("delete UBPM value {key}\\{name}: {error:#}"),
                ),
            }
        }
    }

    let mut removed_registry_keys = 0;
    for relative in profile.software_keys {
        let key = format!("HKLM\\{}\\{}", software_hive_alias, relative);
        match OfflineRegistry::delete_key_verified(&key) {
            Ok(true) => removed_registry_keys += 1,
            Ok(false) => {}
            Err(error) => record_warning(
                &mut warnings,
                format!("delete software key {key}: {error:#}"),
            ),
        }
    }
    for key in &modern_setting_keys {
        match OfflineRegistry::delete_key_verified(key) {
            Ok(true) => removed_registry_keys += 1,
            Ok(false) => {}
            Err(error) => record_warning(
                &mut warnings,
                format!("delete Settings registration {key}: {error:#}"),
            ),
        }
    }
    for control_set in &control_sets {
        for relative in profile.system_keys {
            let key = format!(
                "HKLM\\{}\\ControlSet{:03}\\{}",
                system_hive_alias, control_set, relative
            );
            match OfflineRegistry::delete_key_verified(&key) {
                Ok(true) => removed_registry_keys += 1,
                Ok(false) => {}
                Err(error) => {
                    record_warning(&mut warnings, format!("delete system key {key}: {error:#}"))
                }
            }
        }
        if profile.kind == WindowsUpdateProfileKind::LegacyWua {
            let key = format!(
                "HKLM\\{}\\ControlSet{:03}\\Control\\BackupRestore\\FilesNotToBackup",
                system_hive_alias, control_set
            );
            if OfflineRegistry::key_exists(&key)? {
                match OfflineRegistry::delete_value(&key, "WUA") {
                    Ok(()) => match OfflineRegistry::enumerate_value_names(&key) {
                        Ok(names) if !names.iter().any(|name| name.eq_ignore_ascii_case("WUA")) => {
                        }
                        Ok(_) => record_warning(
                            &mut warnings,
                            format!(
                                "legacy Windows Update backup-exclusion value survived: {key}\\WUA"
                            ),
                        ),
                        Err(error) => record_warning(
                            &mut warnings,
                            format!(
                                "read back legacy backup-exclusion value {key}\\WUA: {error:#}"
                            ),
                        ),
                    },
                    Err(error) => record_warning(
                        &mut warnings,
                        format!("delete legacy backup-exclusion value {key}\\WUA: {error:#}"),
                    ),
                }
            }
        }
    }

    for path in &planned_paths {
        match std::fs::symlink_metadata(path) {
            Ok(_) => record_warning(
                &mut warnings,
                format!(
                    "Windows Update active path survived removal: {}",
                    path.display()
                ),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => record_warning(
                &mut warnings,
                format!(
                    "read back removed Windows Update path {}: {error}",
                    path.display()
                ),
            ),
        }
    }
    for key in &service_keys {
        match OfflineRegistry::key_exists(key) {
            Ok(true) => record_warning(
                &mut warnings,
                format!("Windows Update service survived removal: {key}"),
            ),
            Ok(false) => {}
            Err(error) => record_warning(
                &mut warnings,
                format!("read back Windows Update service {key}: {error:#}"),
            ),
        }
    }
    for (key, id) in &task_ids {
        match OfflineRegistry::key_exists(key) {
            Ok(true) => record_warning(
                &mut warnings,
                format!("Windows Update task tree survived removal: {key}"),
            ),
            Ok(false) => {}
            Err(error) => record_warning(
                &mut warnings,
                format!("read back Windows Update task tree {key}: {error:#}"),
            ),
        }
        if let Some(id) = id {
            for category in ["Tasks", "Plain", "Boot", "Logon", "Maintenance"] {
                let record = format!("{}\\{}\\{}", task_cache_base, category, id);
                match OfflineRegistry::key_exists(&record) {
                    Ok(true) => record_warning(
                        &mut warnings,
                        format!("Windows Update task record survived removal: {record}"),
                    ),
                    Ok(false) => {}
                    Err(error) => record_warning(
                        &mut warnings,
                        format!("read back Windows Update task record {record}: {error:#}"),
                    ),
                }
            }
        }
    }
    for key in &modern_setting_keys {
        match OfflineRegistry::key_exists(key) {
            Ok(true) => record_warning(
                &mut warnings,
                format!("Windows Update Settings registration survived removal: {key}"),
            ),
            Ok(false) => {}
            Err(error) => record_warning(
                &mut warnings,
                format!("read back Windows Update Settings registration {key}: {error:#}"),
            ),
        }
    }
    if let Some(settings_visibility) = &settings_visibility {
        let observed_visibility =
            OfflineRegistry::query_string(&settings_key, SETTINGS_POLICY_VALUE)?;
        if observed_visibility != *settings_visibility
            || !visibility_hides_update_pages(&observed_visibility)
        {
            anyhow::bail!(
                "SettingsPageVisibility readback does not hide every Windows Update page"
            );
        }
    }
    target.verify()?;

    Ok(WindowsUpdateRemovalReport {
        profile: profile.name,
        target_build: build,
        removed_paths,
        removed_services,
        removed_task_trees,
        removed_task_records,
        removed_registry_keys,
        deleted_ubpm_values,
        settings_page_visibility: settings_visibility,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_warning_samples_are_bounded() {
        let mut warnings = Vec::new();
        for index in 0..1000 {
            record_warning(&mut warnings, format!("failure-{index}"));
        }
        assert_eq!(warnings.len(), 32);
        assert_eq!(warnings.first().map(String::as_str), Some("failure-0"));
        assert_eq!(warnings.last().map(String::as_str), Some("failure-31"));
    }

    #[test]
    fn absent_visibility_becomes_exact_hide_list() {
        let value = merge_settings_page_visibility(None).unwrap();
        assert_eq!(
            value,
            "hide:installedupdates;windowsupdate;windowsupdate-action;windowsupdate-history;windowsupdate-options;windowsupdate-restartoptions"
        );
        assert!(visibility_hides_update_pages(&value));
    }

    #[test]
    fn hide_merge_preserves_unrelated_entries_without_duplicates() {
        let value = merge_settings_page_visibility(Some("hide:bluetooth;WindowsUpdate")).unwrap();
        assert!(value.starts_with("hide:bluetooth;WindowsUpdate;"));
        assert_eq!(
            value
                .split(';')
                .filter(|item| item.eq_ignore_ascii_case("windowsupdate"))
                .count(),
            1
        );
        assert!(visibility_hides_update_pages(&value));
    }

    #[test]
    fn showonly_merge_removes_only_update_pages() {
        let value = merge_settings_page_visibility(Some(
            "showonly:display;windowsupdate;bluetooth;WindowsUpdate-History",
        ))
        .unwrap();
        assert_eq!(value, "showonly:display;bluetooth");
        assert!(visibility_hides_update_pages(&value));
    }

    #[test]
    fn malformed_visibility_is_not_overwritten() {
        assert!(merge_settings_page_visibility(Some("windowsupdate;display")).is_err());
    }

    #[test]
    fn task_ids_require_canonical_braced_guid_shape() {
        assert!(is_braced_guid("{01234567-89ab-cdef-0123-456789abcdef}"));
        assert!(!is_braced_guid("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!is_braced_guid("{01234567-89ab-cdef-0123-456789abcdeg}"));
    }

    #[test]
    fn plans_use_only_relative_non_parent_paths() {
        for paths in [LEGACY_ACTIVE_PATHS, MODERN_ACTIVE_PATHS] {
            for relative in paths {
                let path = std::path::Path::new(relative);
                assert!(path.is_relative());
                assert!(!path.components().any(|component| matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )));
            }
        }
    }

    #[test]
    fn localized_resource_names_are_single_safe_file_names() {
        for names in [
            LEGACY_SYSTEM32_LOCALIZED_NAMES,
            LEGACY_SYSWOW64_LOCALIZED_NAMES,
            MODERN_SYSTEM32_LOCALIZED_NAMES,
            MODERN_SYSWOW64_LOCALIZED_NAMES,
        ] {
            for name in names {
                let path = std::path::Path::new(name);
                assert_eq!(path.components().count(), 1, "{name}");
                assert!(path.file_name().is_some(), "{name}");
                assert!(!name.contains(['\\', '/']), "{name}");
            }
        }
    }

    #[test]
    fn supported_client_builds_select_the_expected_component_family() {
        for build in [7_600, 7_601, 9_200, 9_600] {
            assert_eq!(
                profile_for_build(build).unwrap().kind,
                WindowsUpdateProfileKind::LegacyWua
            );
        }
        for build in [
            10_240, 14_393, 17_763, 19_041, 19_045, 22_000, 22_631, 26_100, 28_000,
        ] {
            assert_eq!(
                profile_for_build(build).unwrap().kind,
                WindowsUpdateProfileKind::ModernUso
            );
        }
        for build in [0, 6_001, 9_601, 20_348, 21_999, 29_000] {
            assert!(profile_for_build(build).is_err(), "build {build}");
        }
    }

    #[test]
    fn ubpm_filter_matches_update_names_without_matching_unrelated_maintenance() {
        for name in [
            "CriticalMaintenance_MaintenanceWork",
            "CriticalMeasured_UsoCombinedInstall",
            "CriticalTask_US03",
            "CriticalTask_WUScheduledStart",
            "OOBETask_USOUX02",
        ] {
            assert!(is_update_ubpm_value_name(name), "{name}");
        }
        for name in [
            "CriticalMaintenance_DefenderCleanup",
            "CriticalAction_MsCtfMonitorTask",
            "OOBETask_StoreLicenseMigration",
            "CriticalTask_WiFiTask",
        ] {
            assert!(!is_update_ubpm_value_name(name), "{name}");
        }
    }
}
