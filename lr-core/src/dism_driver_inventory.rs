//! Read-only inventory of driver models staged in an offline Windows image.
//!
//! The Windows implementation dynamically loads the running host's
//! `%SystemRoot%\System32\DismApi.dll`; it never searches the application directory or mixes a
//! newer DISM DLL with an older host/WinPE servicing stack. The DISM driver inventory APIs and the
//! structures used here are available on Windows 7 / Windows Server 2008 R2 and later.
//!
//! DISM is initialized once and serialized for the process lifetime, as required by its global API
//! contract. Each inventory call owns one offline session, copies all returned data into Rust-owned
//! values, releases every DISM allocation, and closes that session. Callers must provide existing
//! writable scratch and log locations; the first initialization establishes those process-global
//! locations for subsequent calls.

use std::path::Path;

const MAX_RECORDED_PACKAGE_FAILURES: usize = 32;
const MAX_PACKAGE_FAILURE_DETAIL_CHARS: usize = 1_024;
const MAX_CLI_DRIVER_PACKAGES: usize = 65_536;
const MAX_CLI_MODELS_PER_PACKAGE: usize = 65_536;

/// Package metadata emitted by the invariant-English DISM command-line inventory.
///
/// This is used only when the running WinPE contains `dism.exe` but omits the optional
/// `DismApi.dll` facade. `class_name` is the INF Class token (for this workflow, `HDC` or
/// `SCSIAdapter`), not a localized class description.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineDriverPackageDescriptor {
    pub published_name: String,
    pub original_file_name: String,
    pub class_name: String,
    pub in_box: bool,
}

/// One image-applicable driver model reported by `DismGetDriverInfo`.
///
/// `boot_critical` and `signature` are diagnostics. They are deliberately not treated as coverage
/// gates: DISM reports an unknown signature for many non-boot-critical packages, and package-level
/// boot criticality does not prove that a package is on this machine's boot-storage path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineDriverCandidate {
    pub published_name: String,
    pub original_file_name: String,
    pub hardware_id: String,
    pub compatible_ids: String,
    pub architecture: u32,
    pub in_box: bool,
    pub boot_critical: bool,
    pub signature: u32,
}

/// A bounded diagnostic for one published package whose model query failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineDriverPackageQueryFailure {
    pub published_name: String,
    pub hresult: u32,
    pub detail: String,
}

/// Partial-safe result of one offline image inventory.
///
/// A package query failure does not invalidate candidates copied from other packages. Callers can
/// therefore accept a required device that is already covered and classify only uncovered devices
/// as unknown when this failure list is nonempty.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OfflineDriverInventory {
    pub candidates: Vec<OfflineDriverCandidate>,
    pub package_query_failures: Vec<OfflineDriverPackageQueryFailure>,
    pub omitted_package_query_failures: usize,
}

fn english_field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    name.trim()
        .eq_ignore_ascii_case(field)
        .then_some(value.trim())
        .filter(|value| !value.is_empty())
}

fn parse_english_yes_no(value: &str, field: &str) -> anyhow::Result<bool> {
    if value.eq_ignore_ascii_case("yes") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("no") {
        Ok(false)
    } else {
        anyhow::bail!("DISM {field} has an invalid invariant-English value: {value}")
    }
}

fn validate_published_inf_name(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if value.len() > 255
        || path.file_name().and_then(|name| name.to_str()) != Some(value)
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("inf"))
    {
        anyhow::bail!("DISM returned an invalid published INF name: {value}");
    }
    Ok(())
}

/// Parses `dism.exe /English /Get-Drivers /All /Format:List` output.
///
/// Microsoft documents these invariant-English field names as the command's report contract.
/// The parser deliberately consumes only package identity, the INF class token, and inbox state;
/// dates, versions, providers, and localized descriptions cannot affect storage-path coverage.
pub fn parse_dism_get_drivers_english(
    output: &str,
) -> anyhow::Result<Vec<OfflineDriverPackageDescriptor>> {
    #[derive(Default)]
    struct PendingPackage {
        published_name: Option<String>,
        original_file_name: Option<String>,
        class_name: Option<String>,
        in_box: Option<bool>,
    }

    fn finish(
        pending: &mut PendingPackage,
        packages: &mut Vec<OfflineDriverPackageDescriptor>,
    ) -> anyhow::Result<()> {
        let Some(published_name) = pending.published_name.take() else {
            return Ok(());
        };
        validate_published_inf_name(&published_name)?;
        let original_file_name = pending.original_file_name.take().ok_or_else(|| {
            anyhow::anyhow!("DISM omitted Original File Name for {published_name}")
        })?;
        let class_name = pending
            .class_name
            .take()
            .ok_or_else(|| anyhow::anyhow!("DISM omitted Class Name for {published_name}"))?;
        let in_box = pending
            .in_box
            .take()
            .ok_or_else(|| anyhow::anyhow!("DISM omitted Inbox for {published_name}"))?;
        if original_file_name.is_empty() || class_name.is_empty() {
            anyhow::bail!("DISM returned empty package metadata for {published_name}");
        }
        packages.push(OfflineDriverPackageDescriptor {
            published_name,
            original_file_name,
            class_name,
            in_box,
        });
        if packages.len() > MAX_CLI_DRIVER_PACKAGES {
            anyhow::bail!(
                "DISM command-line inventory exceeds {MAX_CLI_DRIVER_PACKAGES} driver packages"
            );
        }
        Ok(())
    }

    let mut packages = Vec::new();
    let mut pending = PendingPackage::default();
    for line in output.lines() {
        if let Some(value) = english_field_value(line, "Published Name") {
            finish(&mut pending, &mut packages)?;
            pending = PendingPackage {
                published_name: Some(value.to_owned()),
                ..PendingPackage::default()
            };
        } else if pending.published_name.is_some() {
            if let Some(value) = english_field_value(line, "Original File Name") {
                pending.original_file_name = Some(value.to_owned());
            } else if let Some(value) = english_field_value(line, "Inbox") {
                pending.in_box = Some(parse_english_yes_no(value, "Inbox")?);
            } else if let Some(value) = english_field_value(line, "Class Name") {
                pending.class_name = Some(value.to_owned());
            }
        }
    }
    finish(&mut pending, &mut packages)?;
    if packages.is_empty() {
        anyhow::bail!("DISM /Get-Drivers completed without any package records");
    }

    // Microsoft guarantees unique Oem*.inf published names for third-party packages, but does not
    // make that promise for inbox packages. Real Windows 10 `/Get-Drivers /All` output can contain
    // the same inbox published name (for example ntprint.inf) more than once after component
    // servicing. `/Get-DriverInfo /Driver:<published-name>` remains the authoritative query for the
    // effective image-applicable models, so query each repeated inbox name once. A duplicate that
    // involves an out-of-box package is still inconsistent with DISM's documented Oem*.inf naming.
    let mut unique: Vec<OfflineDriverPackageDescriptor> = Vec::with_capacity(packages.len());
    let mut indexes: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for package in packages {
        let key = package.published_name.to_ascii_lowercase();
        if let Some(&index) = indexes.get(&key) {
            if !package.in_box || !unique[index].in_box {
                anyhow::bail!(
                    "DISM command-line inventory returned duplicate out-of-box published name {}",
                    package.published_name
                );
            }
            continue;
        }
        indexes.insert(key, unique.len());
        unique.push(package);
    }
    Ok(unique)
}

fn dism_architecture_value(value: &str) -> anyhow::Result<u32> {
    // DISM's public DismProcessorArchitecture enum values from DismApi.h. The command has used
    // both `x64` and `amd64` labels across servicing-stack generations.
    if value.eq_ignore_ascii_case("x86") {
        Ok(0)
    } else if value.eq_ignore_ascii_case("arm") {
        Ok(5)
    } else if value.eq_ignore_ascii_case("ia64") {
        Ok(6)
    } else if value.eq_ignore_ascii_case("msil") {
        Ok(8)
    } else if value.eq_ignore_ascii_case("x64") || value.eq_ignore_ascii_case("amd64") {
        Ok(9)
    } else if value.eq_ignore_ascii_case("neutral") {
        Ok(11)
    } else if value.eq_ignore_ascii_case("arm64") {
        Ok(12)
    } else {
        anyhow::bail!("DISM returned an unsupported driver architecture: {value}")
    }
}

/// Parses one successful `dism.exe /English /Get-DriverInfo /Driver:<published-name>` report.
///
/// When a published name is supplied, Microsoft specifies that the report contains only models
/// applicable to the image. `Hardware ID` and `Compatible IDs` therefore have the same coverage
/// meaning as the corresponding `DismDriver` fields returned by the API. They remain separate
/// whole strings so callers can perform only exact device-ID equality; no prefix, suffix, or
/// vendor/device reduction is permitted.
pub fn parse_dism_get_driver_info_english(
    output: &str,
    package: &OfflineDriverPackageDescriptor,
) -> anyhow::Result<Vec<OfflineDriverCandidate>> {
    #[derive(Default)]
    struct PendingModel {
        architecture: Option<u32>,
        hardware_id: String,
        compatible_ids: String,
    }

    fn validate_device_id(value: &str, field: &str, published_name: &str) -> anyhow::Result<()> {
        if value.len() > 32_767 || value.contains(['\r', '\n', '\0']) {
            anyhow::bail!("DISM returned an invalid {field} for {published_name}");
        }
        Ok(())
    }

    fn finish_model(
        pending: &mut PendingModel,
        package: &OfflineDriverPackageDescriptor,
        boot_critical: Option<bool>,
        candidates: &mut Vec<OfflineDriverCandidate>,
        keys: &mut std::collections::HashSet<(String, String, u32)>,
    ) -> anyhow::Result<()> {
        let Some(architecture) = pending.architecture.take() else {
            return Ok(());
        };
        let hardware_id = std::mem::take(&mut pending.hardware_id);
        let compatible_ids = std::mem::take(&mut pending.compatible_ids);
        if hardware_id.is_empty() && compatible_ids.is_empty() {
            return Ok(());
        }
        validate_device_id(&hardware_id, "Hardware ID", &package.published_name)?;
        validate_device_id(&compatible_ids, "Compatible IDs", &package.published_name)?;
        let key = (
            hardware_id.to_ascii_lowercase(),
            compatible_ids.to_ascii_lowercase(),
            architecture,
        );
        if keys.insert(key) {
            candidates.push(OfflineDriverCandidate {
                published_name: package.published_name.clone(),
                original_file_name: package.original_file_name.clone(),
                hardware_id,
                compatible_ids,
                architecture,
                in_box: package.in_box,
                boot_critical: boot_critical.unwrap_or(false),
                // The command report does not expose DismDriverSignature. It is diagnostic only
                // and never participates in the coverage decision.
                signature: 0,
            });
            if candidates.len() > MAX_CLI_MODELS_PER_PACKAGE {
                anyhow::bail!(
                    "DISM command-line inventory exceeds {MAX_CLI_MODELS_PER_PACKAGE} models for {}",
                    package.published_name
                );
            }
        }
        Ok(())
    }

    let mut reported_name = None;
    let mut boot_critical = None;
    let mut pending = PendingModel::default();
    let mut candidates = Vec::new();
    let mut keys = std::collections::HashSet::new();

    for line in output.lines() {
        if let Some(value) = english_field_value(line, "Published Name") {
            if reported_name.replace(value.to_owned()).is_some() {
                anyhow::bail!(
                    "DISM /Get-DriverInfo returned more than one Published Name for {}",
                    package.published_name
                );
            }
        } else if let Some(value) = english_field_value(line, "Boot Critical") {
            boot_critical = Some(parse_english_yes_no(value, "Boot Critical")?);
        } else if let Some(value) = english_field_value(line, "Architecture") {
            finish_model(
                &mut pending,
                package,
                boot_critical,
                &mut candidates,
                &mut keys,
            )?;
            pending.architecture = Some(dism_architecture_value(value)?);
        } else if let Some(hardware_id) = english_field_value(line, "Hardware ID") {
            if pending.architecture.is_none() {
                return Err(anyhow::anyhow!(
                    "DISM returned Hardware ID before Architecture for {}",
                    package.published_name
                ));
            }
            if !pending.hardware_id.is_empty() {
                anyhow::bail!(
                    "DISM returned duplicate Hardware ID for one model in {}",
                    package.published_name
                );
            }
            pending.hardware_id = hardware_id.to_owned();
        } else if let Some(compatible_ids) = english_field_value(line, "Compatible IDs") {
            if pending.architecture.is_none() {
                return Err(anyhow::anyhow!(
                    "DISM returned Compatible IDs before Architecture for {}",
                    package.published_name
                ));
            }
            if !pending.compatible_ids.is_empty() {
                anyhow::bail!(
                    "DISM returned duplicate Compatible IDs for one model in {}",
                    package.published_name
                );
            }
            pending.compatible_ids = compatible_ids.to_owned();
        }
    }
    finish_model(
        &mut pending,
        package,
        boot_critical,
        &mut candidates,
        &mut keys,
    )?;

    let reported_name = reported_name.ok_or_else(|| {
        anyhow::anyhow!(
            "DISM /Get-DriverInfo omitted Published Name for {}",
            package.published_name
        )
    })?;
    if !reported_name.eq_ignore_ascii_case(&package.published_name) {
        anyhow::bail!(
            "DISM /Get-DriverInfo returned Published Name {reported_name} for {}",
            package.published_name
        );
    }
    Ok(candidates)
}

/// Parses concatenated successful invariant-English DISM detail reports.
///
/// Production command execution issues one documented `/Driver:<published-name>` query at a time.
/// This helper only validates and combines already separated report text without implying that
/// multiple `/Driver` switches are accepted by `dism.exe /Get-DriverInfo`.
pub fn parse_dism_get_driver_infos_english(
    output: &str,
    packages: &[OfflineDriverPackageDescriptor],
) -> anyhow::Result<Vec<OfflineDriverCandidate>> {
    if packages.is_empty() {
        anyhow::bail!("no published driver packages were supplied for DISM detail parsing");
    }
    let package_by_name = packages
        .iter()
        .map(|package| (package.published_name.to_ascii_lowercase(), package))
        .collect::<std::collections::HashMap<_, _>>();
    if package_by_name.len() != packages.len() {
        anyhow::bail!("duplicate published names were supplied for DISM detail parsing");
    }

    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current_name: Option<String> = None;
    let mut current_report = String::new();
    let finish = |name: Option<String>,
                  report: &mut String,
                  candidates: &mut Vec<OfflineDriverCandidate>,
                  seen: &mut std::collections::HashSet<String>|
     -> anyhow::Result<()> {
        let Some(name) = name else {
            return Ok(());
        };
        let key = name.to_ascii_lowercase();
        let package = package_by_name
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("DISM returned unrequested Published Name {name}"))?;
        if !seen.insert(key) {
            anyhow::bail!("DISM returned duplicate detail report for {name}");
        }
        candidates.extend(parse_dism_get_driver_info_english(report, package)?);
        report.clear();
        Ok(())
    };

    for line in output.lines() {
        if let Some(value) = english_field_value(line, "Published Name") {
            finish(
                current_name.take(),
                &mut current_report,
                &mut candidates,
                &mut seen,
            )?;
            current_name = Some(value.to_owned());
        }
        if current_name.is_some() {
            current_report.push_str(line);
            current_report.push('\n');
        }
    }
    finish(
        current_name,
        &mut current_report,
        &mut candidates,
        &mut seen,
    )?;
    if seen.len() != packages.len() {
        let missing = packages
            .iter()
            .filter(|package| !seen.contains(&package.published_name.to_ascii_lowercase()))
            .map(|package| package.published_name.as_str())
            .take(4)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "DISM detail report returned {} of {} requested packages; missing sample: {missing}",
            seen.len(),
            packages.len()
        );
    }
    Ok(candidates)
}

/// Finds image-applicable coverage for a device.
///
/// A single case-insensitive, whole-string match is sufficient. `device_ids` may contain both
/// hardware IDs and compatible IDs obtained from the source device. Both corresponding candidate
/// fields participate independently; neither side is shortened or treated as a prefix. Architecture
/// does not need a second filter here because querying an installed published name makes DISM
/// return only models applicable to that image.
pub fn find_offline_driver_coverage<'a>(
    candidates: &'a [OfflineDriverCandidate],
    device_ids: &[String],
) -> Option<&'a OfflineDriverCandidate> {
    candidates.iter().find(|candidate| {
        device_ids.iter().any(|device_id| {
            !device_id.is_empty()
                && ((!candidate.hardware_id.is_empty()
                    && candidate.hardware_id.eq_ignore_ascii_case(device_id))
                    || (!candidate.compatible_ids.is_empty()
                        && candidate.compatible_ids.eq_ignore_ascii_case(device_id)))
        })
    })
}

/// Enumerates all image-applicable driver models staged in an offline Windows image.
///
/// `image_root`, `scratch_dir`, and `log_path` must be absolute. The image and scratch directories
/// and the log parent must already exist as ordinary (non-reparse-point) directories. The scratch
/// directory and log destination must be writable by the caller; DISM remains the authority for
/// actual access failures. Returned strings are copied before their owning DISM allocations are
/// released. A failed `DismGetDriverInfo` for one published package is reported in the returned
/// bounded diagnostics while other package candidates are retained. DISM initialization, package
/// list enumeration, ABI/structure validation, allocation cleanup, and session-close failures
/// remain hard errors.
#[cfg(windows)]
pub fn enumerate_offline_driver_candidates(
    image_root: &Path,
    scratch_dir: &Path,
    log_path: &Path,
) -> anyhow::Result<OfflineDriverInventory> {
    windows_impl::enumerate(image_root, scratch_dir, log_path)
}

/// DISM API is a Windows desktop API (minimum supported client: Windows 7).
#[cfg(not(windows))]
pub fn enumerate_offline_driver_candidates(
    image_root: &Path,
    scratch_dir: &Path,
    log_path: &Path,
) -> anyhow::Result<OfflineDriverInventory> {
    let _ = (image_root, scratch_dir, log_path);
    anyhow::bail!(
        "offline DISM driver inventory is unsupported on this platform; Windows 7 or later is required"
    )
}

fn combine_primary_and_cleanup<T>(
    primary: anyhow::Result<T>,
    cleanup_errors: Vec<String>,
) -> anyhow::Result<T> {
    if cleanup_errors.is_empty() {
        return primary;
    }
    let cleanup = cleanup_errors.join("; ");
    match primary {
        Ok(_) => anyhow::bail!("DISM inventory cleanup failed: {cleanup}"),
        Err(error) => Err(anyhow::anyhow!(
            "{error:#}; DISM inventory cleanup also failed: {cleanup}"
        )),
    }
}

fn push_unique_candidate(
    candidates: &mut Vec<OfflineDriverCandidate>,
    seen: &mut std::collections::HashSet<(String, String, String, u32)>,
    candidate: OfflineDriverCandidate,
) {
    let key = (
        candidate.published_name.to_ascii_lowercase(),
        candidate.hardware_id.to_ascii_lowercase(),
        candidate.compatible_ids.to_ascii_lowercase(),
        candidate.architecture,
    );
    if seen.insert(key) {
        candidates.push(candidate);
    }
}

fn record_package_query_failure(
    failures: &mut Vec<OfflineDriverPackageQueryFailure>,
    omitted: &mut usize,
    failure: OfflineDriverPackageQueryFailure,
) {
    if failures.len() < MAX_RECORDED_PACKAGE_FAILURES {
        failures.push(failure);
    } else {
        *omitted = omitted.saturating_add(1);
    }
}

fn bound_detail(detail: String) -> String {
    if detail.chars().count() <= MAX_PACKAGE_FAILURE_DETAIL_CHARS {
        return detail;
    }
    let mut bounded: String = detail
        .chars()
        .take(MAX_PACKAGE_FAILURE_DETAIL_CHARS - 1)
        .collect();
    bounded.push('…');
    bounded
}

#[cfg(windows)]
mod windows_impl {
    use super::{
        bound_detail, combine_primary_and_cleanup, push_unique_candidate,
        record_package_query_failure, OfflineDriverCandidate, OfflineDriverInventory,
        OfflineDriverPackageQueryFailure,
    };
    use anyhow::{anyhow, bail, Context};
    use libloading::Library;
    use std::collections::{HashMap, HashSet};
    use std::ffi::{c_void, OsStr};
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::path::Path;
    use std::ptr;
    use std::slice;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    const S_OK: i32 = 0;
    const TRUE: i32 = 1;
    const DISM_LOG_ERRORS_WARNINGS_INFO: i32 = 2;
    const MAX_PATH_CODE_UNITS: usize = 32_767;
    const MAX_DISM_STRING_CODE_UNITS: usize = 32_767;
    const MAX_DRIVER_PACKAGES: usize = 65_536;
    const MAX_MODELS_PER_PACKAGE: usize = 65_536;
    const MAX_TOTAL_CANDIDATES: usize = 262_144;

    // Microsoft defines DismInitialize as once-per-process and DismShutdown as the terminal end of
    // all DISM API use in that process. Keep one initialized, serialized API instance alive for the
    // remaining process lifetime so retries and repeated offline exports never call DISM after a
    // prior shutdown. Process teardown reclaims the servicing runtime and loaded module; calling
    // DismShutdown earlier would make every later query contract-invalid.
    static DISM_PROCESS_API: OnceLock<Mutex<Option<DismApi>>> = OnceLock::new();

    type DismSession = u32;
    type DismInitializeFn = unsafe extern "system" fn(i32, *const u16, *const u16) -> i32;
    type DismOpenSessionFn =
        unsafe extern "system" fn(*const u16, *const u16, *const u16, *mut DismSession) -> i32;
    type DismGetDriversFn =
        unsafe extern "system" fn(DismSession, i32, *mut *mut DismDriverPackage, *mut u32) -> i32;
    type DismGetDriverInfoFn = unsafe extern "system" fn(
        DismSession,
        *const u16,
        *mut *mut DismDriver,
        *mut u32,
        *mut *mut DismDriverPackage,
    ) -> i32;
    type DismDeleteFn = unsafe extern "system" fn(*mut c_void) -> i32;
    type DismGetLastErrorMessageFn = unsafe extern "system" fn(*mut *mut DismString) -> i32;
    type DismCloseSessionFn = unsafe extern "system" fn(DismSession) -> i32;

    // DismApi.h wraps DISM's returned structures in #pragma pack(push, 1). Keep these private ABI
    // mirrors packed and read every field with read_unaligned; never create a reference to a field.
    #[repr(C, packed(1))]
    struct DismSystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C, packed(1))]
    struct DismDriverPackage {
        published_name: *const u16,
        original_file_name: *const u16,
        in_box: i32,
        catalog_file: *const u16,
        class_name: *const u16,
        class_guid: *const u16,
        class_description: *const u16,
        boot_critical: i32,
        driver_signature: i32,
        provider_name: *const u16,
        date: DismSystemTime,
        major_version: u32,
        minor_version: u32,
        build: u32,
        revision: u32,
    }

    #[repr(C, packed(1))]
    struct DismDriver {
        manufacturer_name: *const u16,
        hardware_description: *const u16,
        hardware_id: *const u16,
        architecture: u32,
        service_name: *const u16,
        compatible_ids: *const u16,
        exclude_ids: *const u16,
    }

    #[repr(C, packed(1))]
    struct DismString {
        value: *const u16,
    }

    // Compile-time guards against accidental ABI drift from the packed DismApi.h definitions.
    #[cfg(target_pointer_width = "64")]
    const _: [(); 100] = [(); std::mem::size_of::<DismDriverPackage>()];
    #[cfg(target_pointer_width = "64")]
    const _: [(); 52] = [(); std::mem::size_of::<DismDriver>()];
    #[cfg(target_pointer_width = "64")]
    const _: [(); 8] = [(); std::mem::size_of::<DismString>()];
    #[cfg(target_pointer_width = "32")]
    const _: [(); 72] = [(); std::mem::size_of::<DismDriverPackage>()];
    #[cfg(target_pointer_width = "32")]
    const _: [(); 28] = [(); std::mem::size_of::<DismDriver>()];
    #[cfg(target_pointer_width = "32")]
    const _: [(); 4] = [(); std::mem::size_of::<DismString>()];

    struct DismApi {
        _library: Library,
        initialize: DismInitializeFn,
        open_session: DismOpenSessionFn,
        get_drivers: DismGetDriversFn,
        get_driver_info: DismGetDriverInfoFn,
        delete: DismDeleteFn,
        get_last_error_message: DismGetLastErrorMessageFn,
        close_session: DismCloseSessionFn,
    }

    #[derive(Clone)]
    struct PackageMetadata {
        published_name: String,
        original_file_name: String,
        in_box: bool,
        boot_critical: bool,
        signature: u32,
    }

    #[derive(Default)]
    struct InventoryAccumulator {
        candidates: Vec<OfflineDriverCandidate>,
        candidate_keys: HashSet<(String, String, String, u32)>,
        total_models_seen: usize,
        package_query_failures: Vec<OfflineDriverPackageQueryFailure>,
        omitted_package_query_failures: usize,
    }

    struct CapturedDismFailure {
        message: String,
        cleanup_errors: Vec<String>,
    }

    impl CapturedDismFailure {
        fn into_error(self) -> anyhow::Error {
            combine_primary_and_cleanup::<()>(Err(anyhow!(self.message)), self.cleanup_errors)
                .expect_err("a captured DISM failure must remain an error")
        }
    }

    impl DismApi {
        unsafe fn load() -> anyhow::Result<Self> {
            let system_directory = crate::windows_compat::system_directory()
                .context("GetSystemDirectoryW failed while locating host DismApi.dll")?;
            if !system_directory.is_absolute() {
                bail!(
                    "GetSystemDirectoryW returned a non-absolute directory: {}",
                    system_directory.display()
                );
            }
            let dll_path = system_directory.join("DismApi.dll");
            validate_existing_regular_file(&dll_path, "host DismApi.dll")?;
            let library = Library::new(&dll_path).with_context(|| {
                format!(
                    "loading the host servicing DLL failed: {}",
                    dll_path.display()
                )
            })?;

            unsafe fn resolve<T: Copy>(
                library: &Library,
                name: &'static [u8],
            ) -> anyhow::Result<T> {
                Ok(*library.get::<T>(name).with_context(|| {
                    format!(
                        "host DismApi.dll does not export {}",
                        String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
                    )
                })?)
            }

            let initialize = resolve(&library, b"DismInitialize\0")?;
            let open_session = resolve(&library, b"DismOpenSession\0")?;
            let get_drivers = resolve(&library, b"DismGetDrivers\0")?;
            let get_driver_info = resolve(&library, b"DismGetDriverInfo\0")?;
            let delete = resolve(&library, b"DismDelete\0")?;
            let get_last_error_message = resolve(&library, b"DismGetLastErrorMessage\0")?;
            let close_session = resolve(&library, b"DismCloseSession\0")?;

            Ok(Self {
                _library: library,
                initialize,
                open_session,
                get_drivers,
                get_driver_info,
                delete,
                get_last_error_message,
                close_session,
            })
        }

        unsafe fn capture_failed_call(&self, operation: &str, hresult: i32) -> CapturedDismFailure {
            // Microsoft requires this query on the same thread immediately after the failed DISM
            // call. Do not insert another DISM call before it.
            let mut message_ptr: *mut DismString = ptr::null_mut();
            let message_hr = (self.get_last_error_message)(&mut message_ptr);
            let mut detail = None;
            let mut detail_error = None;
            let mut cleanup_errors = Vec::new();
            if message_hr == S_OK {
                if message_ptr.is_null() {
                    detail_error = Some("DismGetLastErrorMessage returned a null structure".into());
                } else {
                    let value_ptr = ptr::addr_of!((*message_ptr).value).read_unaligned();
                    match copy_required_utf16(value_ptr, "DISM last error message") {
                        Ok(value) => detail = Some(value),
                        Err(error) => detail_error = Some(format!("{error:#}")),
                    }
                    let delete_hr = (self.delete)(message_ptr.cast());
                    if delete_hr != S_OK {
                        cleanup_errors.push(format!(
                            "DismDelete(last error message) returned {}",
                            format_hresult(delete_hr)
                        ));
                    }
                }
            } else {
                detail_error = Some(format!(
                    "DismGetLastErrorMessage returned {}",
                    format_hresult(message_hr)
                ));
            }

            let mut message = format!("{operation} failed with {}", format_hresult(hresult));
            if let Some(detail) = detail {
                message.push_str(": ");
                message.push_str(&detail);
            }
            if let Some(detail_error) = detail_error {
                message.push_str("; detailed error retrieval/cleanup failed: ");
                message.push_str(&detail_error);
            }
            CapturedDismFailure {
                message,
                cleanup_errors,
            }
        }

        unsafe fn failed_call(&self, operation: &str, hresult: i32) -> anyhow::Error {
            self.capture_failed_call(operation, hresult).into_error()
        }

        unsafe fn delete_output(&self, pointer: *mut c_void, label: &str) -> Option<String> {
            if pointer.is_null() {
                return None;
            }
            let hresult = (self.delete)(pointer);
            (hresult != S_OK)
                .then(|| format!("DismDelete({label}) returned {}", format_hresult(hresult)))
        }
    }

    fn process_api(
        scratch_wide: &[u16],
        log_wide: &[u16],
    ) -> anyhow::Result<MutexGuard<'static, Option<DismApi>>> {
        let process_state = DISM_PROCESS_API.get_or_init(|| Mutex::new(None));
        let mut state = process_state
            .lock()
            .map_err(|_| anyhow!("process DISM API lock was poisoned by an earlier panic"))?;
        if state.is_none() {
            let api = unsafe { DismApi::load()? };
            let initialize_hr = unsafe {
                (api.initialize)(
                    DISM_LOG_ERRORS_WARNINGS_INFO,
                    log_wide.as_ptr(),
                    scratch_wide.as_ptr(),
                )
            };
            if initialize_hr != S_OK {
                // A failed initialization did not establish process state, so leave the slot empty
                // and allow an explicit later user retry. Once success is stored, it is never
                // initialized or shut down again during this process.
                return Err(unsafe { api.failed_call("DismInitialize", initialize_hr) });
            }
            *state = Some(api);
        }
        Ok(state)
    }

    pub(super) fn enumerate(
        image_root: &Path,
        scratch_dir: &Path,
        log_path: &Path,
    ) -> anyhow::Result<OfflineDriverInventory> {
        validate_inputs(image_root, scratch_dir, log_path)?;
        let image_wide = path_to_wide(image_root, "offline image root")?;
        let scratch_wide = path_to_wide(scratch_dir, "DISM scratch directory")?;
        let log_wide = path_to_wide(log_path, "DISM log path")?;

        let api_guard = process_api(&scratch_wide, &log_wide)?;
        let api = api_guard
            .as_ref()
            .ok_or_else(|| anyhow!("process DISM API state is unexpectedly empty"))?;

        let mut session = None;
        let primary = (|| -> anyhow::Result<OfflineDriverInventory> {
            let mut raw_session = 0_u32;
            let open_hr = unsafe {
                (api.open_session)(
                    image_wide.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    &mut raw_session,
                )
            };
            if open_hr != S_OK {
                return Err(unsafe { api.failed_call("DismOpenSession", open_hr) });
            }
            if raw_session == 0 {
                bail!("DismOpenSession returned S_OK with DISM_SESSION_DEFAULT");
            }
            session = Some(raw_session);
            enumerate_session(api, raw_session)
        })();

        let mut cleanup_errors = Vec::new();
        if let Some(session) = session {
            let close_hr = unsafe { (api.close_session)(session) };
            if close_hr != S_OK {
                cleanup_errors.push(format!("{}", unsafe {
                    api.failed_call("DismCloseSession", close_hr)
                }));
            }
        }
        combine_primary_and_cleanup(primary, cleanup_errors)
    }

    fn enumerate_session(
        api: &DismApi,
        session: DismSession,
    ) -> anyhow::Result<OfflineDriverInventory> {
        let mut packages_ptr: *mut DismDriverPackage = ptr::null_mut();
        let mut package_count = 0_u32;
        let get_hr =
            unsafe { (api.get_drivers)(session, TRUE, &mut packages_ptr, &mut package_count) };
        if get_hr != S_OK {
            // Capture thread-local detail before any other DISM call, then release any output the
            // failed call still returned. DISM does not promise failure leaves outputs null.
            let mut captured =
                unsafe { api.capture_failed_call("DismGetDrivers(AllDrivers=TRUE)", get_hr) };
            if let Some(error) =
                unsafe { api.delete_output(packages_ptr.cast(), "failed driver package array") }
            {
                captured.cleanup_errors.push(error);
            }
            return Err(captured.into_error());
        }

        let primary = (|| -> anyhow::Result<OfflineDriverInventory> {
            let package_count =
                checked_count(package_count, MAX_DRIVER_PACKAGES, "driver packages")?;
            if package_count != 0 && packages_ptr.is_null() {
                bail!("DismGetDrivers returned a nonzero count with a null package array");
            }
            let packages = if package_count == 0 {
                &[][..]
            } else {
                unsafe { slice::from_raw_parts(packages_ptr, package_count) }
            };

            let mut published_names = HashMap::new();
            let mut inventory = InventoryAccumulator::default();
            for package in packages {
                let metadata = unsafe { copy_package_metadata(package)? };
                let published_key = metadata.published_name.to_ascii_lowercase();
                if let Some(previous_in_box) = published_names.get(&published_key) {
                    if !metadata.in_box || !previous_in_box {
                        bail!(
                            "DismGetDrivers returned duplicate out-of-box published name {}",
                            metadata.published_name
                        );
                    }
                    // Inbox published names are not documented as unique and can repeat in a
                    // serviced image. The first query by this name already asks DISM for the
                    // effective image-applicable models, so a second identical name adds no
                    // authority and must not turn a valid image into a terminal failure.
                    continue;
                }
                published_names.insert(published_key, metadata.in_box);
                enumerate_package_models(api, session, &metadata, &mut inventory)?;
            }
            Ok(OfflineDriverInventory {
                candidates: inventory.candidates,
                package_query_failures: inventory.package_query_failures,
                omitted_package_query_failures: inventory.omitted_package_query_failures,
            })
        })();

        let mut cleanup_errors = Vec::new();
        if let Some(error) =
            unsafe { api.delete_output(packages_ptr.cast(), "driver package array") }
        {
            cleanup_errors.push(error);
        }
        combine_primary_and_cleanup(primary, cleanup_errors)
    }

    fn enumerate_package_models(
        api: &DismApi,
        session: DismSession,
        listed_metadata: &PackageMetadata,
        inventory: &mut InventoryAccumulator,
    ) -> anyhow::Result<()> {
        let published_wide =
            string_to_wide(&listed_metadata.published_name, "published driver name")?;
        let mut drivers_ptr: *mut DismDriver = ptr::null_mut();
        let mut driver_count = 0_u32;
        let mut package_ptr: *mut DismDriverPackage = ptr::null_mut();
        let info_hr = unsafe {
            (api.get_driver_info)(
                session,
                published_wide.as_ptr(),
                &mut drivers_ptr,
                &mut driver_count,
                &mut package_ptr,
            )
        };
        if info_hr != S_OK {
            // Capture DISM's thread-local detail before formatting or performing any other work.
            let mut captured = unsafe { api.capture_failed_call("DismGetDriverInfo", info_hr) };
            if let Some(error) =
                unsafe { api.delete_output(drivers_ptr.cast(), "failed driver model array") }
            {
                captured.cleanup_errors.push(error);
            }
            if let Some(error) =
                unsafe { api.delete_output(package_ptr.cast(), "failed driver package info") }
            {
                captured.cleanup_errors.push(error);
            }
            if !captured.cleanup_errors.is_empty() {
                return Err(captured.into_error()).with_context(|| {
                    format!(
                        "querying staged driver package {} failed",
                        listed_metadata.published_name
                    )
                });
            }
            record_package_query_failure(
                &mut inventory.package_query_failures,
                &mut inventory.omitted_package_query_failures,
                OfflineDriverPackageQueryFailure {
                    published_name: listed_metadata.published_name.clone(),
                    hresult: info_hr as u32,
                    detail: bound_detail(captured.message),
                },
            );
            return Ok(());
        }

        let primary = (|| -> anyhow::Result<()> {
            let driver_count = checked_count(
                driver_count,
                MAX_MODELS_PER_PACKAGE,
                "driver models in one package",
            )?;
            inventory.total_models_seen = inventory
                .total_models_seen
                .checked_add(driver_count)
                .ok_or_else(|| anyhow!("DISM driver model count overflowed usize"))?;
            if inventory.total_models_seen > MAX_TOTAL_CANDIDATES {
                bail!(
                    "DISM driver inventory exceeds the bounded total of {MAX_TOTAL_CANDIDATES} models"
                );
            }
            if driver_count != 0 && drivers_ptr.is_null() {
                bail!("DismGetDriverInfo returned a nonzero count with a null driver array");
            }
            if package_ptr.is_null() {
                bail!("DismGetDriverInfo returned S_OK with a null driver package");
            }
            let metadata = unsafe { copy_package_metadata(&*package_ptr)? };
            if !metadata
                .published_name
                .eq_ignore_ascii_case(&listed_metadata.published_name)
            {
                bail!(
                    "DismGetDriverInfo returned published name {} for requested {}",
                    metadata.published_name,
                    listed_metadata.published_name
                );
            }

            let drivers = if driver_count == 0 {
                &[][..]
            } else {
                unsafe { slice::from_raw_parts(drivers_ptr, driver_count) }
            };
            for driver in drivers {
                let hardware_id_ptr = unsafe { ptr::addr_of!(driver.hardware_id).read_unaligned() };
                let compatible_ids_ptr =
                    unsafe { ptr::addr_of!(driver.compatible_ids).read_unaligned() };
                let architecture = unsafe { ptr::addr_of!(driver.architecture).read_unaligned() };
                let hardware_id =
                    unsafe { copy_optional_utf16(hardware_id_ptr, "DismDriver.HardwareId")? };
                let compatible_ids =
                    unsafe { copy_optional_utf16(compatible_ids_ptr, "DismDriver.CompatibleIds")? };
                if hardware_id.is_empty() && compatible_ids.is_empty() {
                    bail!("DismDriver has neither HardwareId nor CompatibleIds");
                }
                push_unique_candidate(
                    &mut inventory.candidates,
                    &mut inventory.candidate_keys,
                    OfflineDriverCandidate {
                        published_name: metadata.published_name.clone(),
                        original_file_name: metadata.original_file_name.clone(),
                        hardware_id,
                        compatible_ids,
                        architecture,
                        in_box: metadata.in_box,
                        boot_critical: metadata.boot_critical,
                        signature: metadata.signature,
                    },
                );
            }
            Ok(())
        })();

        let mut cleanup_errors = Vec::new();
        if let Some(error) = unsafe { api.delete_output(drivers_ptr.cast(), "driver model array") }
        {
            cleanup_errors.push(error);
        }
        if let Some(error) = unsafe { api.delete_output(package_ptr.cast(), "driver package info") }
        {
            cleanup_errors.push(error);
        }
        combine_primary_and_cleanup(primary, cleanup_errors)
    }

    unsafe fn copy_package_metadata(
        package: &DismDriverPackage,
    ) -> anyhow::Result<PackageMetadata> {
        let published_ptr = ptr::addr_of!(package.published_name).read_unaligned();
        let original_ptr = ptr::addr_of!(package.original_file_name).read_unaligned();
        let in_box = ptr::addr_of!(package.in_box).read_unaligned();
        let boot_critical = ptr::addr_of!(package.boot_critical).read_unaligned();
        let signature = ptr::addr_of!(package.driver_signature).read_unaligned();
        if !(0..=2).contains(&signature) {
            bail!("DismDriverPackage.DriverSignature has invalid value {signature}");
        }
        let published_name = copy_required_utf16(published_ptr, "published driver name")?;
        let original_file_name = copy_required_utf16(original_ptr, "original driver file name")?;
        if published_name.is_empty() {
            bail!("DismDriverPackage.PublishedName is empty");
        }
        if original_file_name.is_empty() {
            bail!("DismDriverPackage.OriginalFileName is empty");
        }
        Ok(PackageMetadata {
            published_name,
            original_file_name,
            in_box: in_box != 0,
            boot_critical: boot_critical != 0,
            signature: signature as u32,
        })
    }

    unsafe fn copy_required_utf16(pointer: *const u16, label: &str) -> anyhow::Result<String> {
        if pointer.is_null() {
            bail!("{label} is null");
        }
        let mut length = 0_usize;
        while length < MAX_DISM_STRING_CODE_UNITS {
            if pointer.add(length).read() == 0 {
                let units = slice::from_raw_parts(pointer, length);
                return String::from_utf16(units)
                    .with_context(|| format!("{label} is not valid UTF-16"));
            }
            length += 1;
        }
        bail!("{label} is not NUL-terminated within {MAX_DISM_STRING_CODE_UNITS} UTF-16 code units")
    }

    unsafe fn copy_optional_utf16(pointer: *const u16, label: &str) -> anyhow::Result<String> {
        if pointer.is_null() {
            return Ok(String::new());
        }
        copy_required_utf16(pointer, label)
    }

    fn checked_count(value: u32, maximum: usize, label: &str) -> anyhow::Result<usize> {
        let value = value as usize;
        if value > maximum {
            bail!("DISM returned {value} {label}; maximum accepted is {maximum}");
        }
        Ok(value)
    }

    fn validate_inputs(
        image_root: &Path,
        scratch_dir: &Path,
        log_path: &Path,
    ) -> anyhow::Result<()> {
        validate_existing_directory(image_root, "offline image root")?;
        validate_existing_directory(scratch_dir, "DISM scratch directory")?;
        validate_existing_directory(
            &image_root.join("Windows"),
            "offline image Windows directory",
        )?;
        validate_absolute(log_path, "DISM log path")?;
        let log_parent = log_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| anyhow!("DISM log path has no parent directory"))?;
        validate_existing_directory(log_parent, "DISM log parent directory")?;
        match fs::symlink_metadata(log_path) {
            Ok(metadata) => {
                reject_reparse(&metadata, log_path, "DISM log path")?;
                if !metadata.is_file() {
                    bail!(
                        "DISM log path is not an ordinary file: {}",
                        log_path.display()
                    );
                }
                if metadata.permissions().readonly() {
                    bail!("DISM log path is read-only: {}", log_path.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "reading DISM log path metadata failed: {}",
                        log_path.display()
                    )
                });
            }
        }
        path_to_wide(image_root, "offline image root")?;
        path_to_wide(scratch_dir, "DISM scratch directory")?;
        path_to_wide(log_path, "DISM log path")?;
        Ok(())
    }

    fn validate_existing_directory(path: &Path, label: &str) -> anyhow::Result<()> {
        validate_absolute(path, label)?;
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("reading {label} metadata failed: {}", path.display()))?;
        reject_reparse(&metadata, path, label)?;
        if !metadata.is_dir() {
            bail!("{label} is not an ordinary directory: {}", path.display());
        }
        Ok(())
    }

    fn validate_existing_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
        validate_absolute(path, label)?;
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("reading {label} metadata failed: {}", path.display()))?;
        reject_reparse(&metadata, path, label)?;
        if !metadata.is_file() {
            bail!("{label} is not an ordinary file: {}", path.display());
        }
        Ok(())
    }

    fn reject_reparse(metadata: &fs::Metadata, path: &Path, label: &str) -> anyhow::Result<()> {
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            bail!("{label} is a reparse point: {}", path.display());
        }
        Ok(())
    }

    fn validate_absolute(path: &Path, label: &str) -> anyhow::Result<()> {
        if !path.is_absolute() {
            bail!("{label} must be absolute: {}", path.display());
        }
        Ok(())
    }

    fn path_to_wide(path: &Path, label: &str) -> anyhow::Result<Vec<u16>> {
        os_string_to_wide(path.as_os_str(), label)
    }

    fn string_to_wide(value: &str, label: &str) -> anyhow::Result<Vec<u16>> {
        os_string_to_wide(OsStr::new(value), label)
    }

    fn os_string_to_wide(value: &OsStr, label: &str) -> anyhow::Result<Vec<u16>> {
        let mut wide: Vec<u16> = value.encode_wide().collect();
        if wide.is_empty() {
            bail!("{label} is empty");
        }
        if wide.len() > MAX_PATH_CODE_UNITS {
            bail!("{label} exceeds {MAX_PATH_CODE_UNITS} UTF-16 code units");
        }
        if wide.contains(&0) {
            bail!("{label} contains an embedded NUL");
        }
        wide.push(0);
        Ok(wide)
    }

    fn format_hresult(hresult: i32) -> String {
        format!("0x{:08X}", hresult as u32)
    }

    #[cfg(test)]
    mod tests {
        use super::{checked_count, copy_optional_utf16};

        #[test]
        fn count_bounds_accept_limit_and_reject_excess() {
            assert_eq!(checked_count(4, 4, "items").unwrap(), 4);
            assert!(checked_count(5, 4, "items").is_err());
        }

        #[test]
        fn optional_driver_id_accepts_null_and_copies_present_utf16() {
            assert_eq!(
                unsafe { copy_optional_utf16(std::ptr::null(), "optional ID") }.unwrap(),
                ""
            );
            let compatible_id = "PCI\\CC_0106\0".encode_utf16().collect::<Vec<_>>();
            assert_eq!(
                unsafe { copy_optional_utf16(compatible_id.as_ptr(), "compatible ID") }.unwrap(),
                "PCI\\CC_0106"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        bound_detail, combine_primary_and_cleanup, find_offline_driver_coverage,
        parse_dism_get_driver_info_english, parse_dism_get_driver_infos_english,
        parse_dism_get_drivers_english, push_unique_candidate, record_package_query_failure,
        OfflineDriverCandidate, OfflineDriverInventory, OfflineDriverPackageQueryFailure,
        MAX_PACKAGE_FAILURE_DETAIL_CHARS, MAX_RECORDED_PACKAGE_FAILURES,
    };

    fn candidate(
        published_name: &str,
        hardware_id: &str,
        architecture: u32,
        in_box: bool,
    ) -> OfflineDriverCandidate {
        OfflineDriverCandidate {
            published_name: published_name.into(),
            original_file_name: format!("source-{published_name}"),
            hardware_id: hardware_id.into(),
            compatible_ids: String::new(),
            architecture,
            in_box,
            boot_critical: false,
            signature: 0,
        }
    }

    #[test]
    fn finds_inbox_coverage_by_exact_case_insensitive_id() {
        let candidates = vec![candidate("storahci.inf", "PCI\\CC_010601", 9, true)];
        let ids = vec!["pci\\cc_010601".into()];
        let found = find_offline_driver_coverage(&candidates, &ids).unwrap();
        assert!(found.in_box);
        assert_eq!(found.published_name, "storahci.inf");
    }

    #[test]
    fn finds_same_oem_package_already_present() {
        let candidates = vec![candidate("oem42.inf", "PCI\\VEN_8086&DEV_2822", 9, false)];
        let ids = vec!["PCI\\VEN_8086&DEV_2822".into()];
        assert_eq!(
            find_offline_driver_coverage(&candidates, &ids)
                .unwrap()
                .published_name,
            "oem42.inf"
        );
    }

    #[test]
    fn source_compatible_id_can_match_candidate_hardware_id() {
        let candidates = vec![candidate("storahci.inf", "PCI\\CC_0106", 9, true)];
        let source_hardware_and_compatible_ids =
            vec!["PCI\\VEN_8086&DEV_A102".into(), "PCI\\CC_0106".into()];
        assert!(
            find_offline_driver_coverage(&candidates, &source_hardware_and_compatible_ids)
                .is_some()
        );
    }

    #[test]
    fn candidate_compatible_id_can_match_a_source_device_id() {
        let mut compatible_only = candidate("storahci.inf", "", 9, true);
        compatible_only.compatible_ids = "PCI\\CC_0106".into();
        let source_hardware_and_compatible_ids =
            vec!["PCI\\VEN_8086&DEV_A102".into(), "pci\\cc_0106".into()];

        assert!(find_offline_driver_coverage(
            &[compatible_only],
            &source_hardware_and_compatible_ids
        )
        .is_some());
    }

    #[test]
    fn wrong_architecture_record_does_not_match_a_different_id() {
        // Architecture is diagnostic here because DismGetDriverInfo already scopes installed
        // published names to the target image. A wrong-architecture-looking record must not create
        // coverage merely because it exists; the exact ID still has to match.
        let candidates = vec![candidate("legacy.inf", "PCI\\VEN_1234&DEV_0001", 0, false)];
        let ids = vec!["PCI\\VEN_8086&DEV_2822".into()];
        assert!(find_offline_driver_coverage(&candidates, &ids).is_none());
    }

    #[test]
    fn partial_or_empty_ids_do_not_match() {
        let candidates = vec![candidate("storahci.inf", "PCI\\CC_010601", 9, true)];
        assert!(find_offline_driver_coverage(&candidates, &["PCI\\CC_0106".into()]).is_none());
        assert!(
            find_offline_driver_coverage(&candidates, &["PCI\\CC_010601&REV_01".into()]).is_none()
        );
        assert!(find_offline_driver_coverage(&candidates, &[String::new()]).is_none());
    }

    #[test]
    fn duplicate_candidates_are_collapsed_case_insensitively() {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        push_unique_candidate(
            &mut candidates,
            &mut seen,
            candidate("OEM7.INF", "PCI\\VEN_8086&DEV_2822", 9, false),
        );
        push_unique_candidate(
            &mut candidates,
            &mut seen,
            candidate("oem7.inf", "pci\\ven_8086&dev_2822", 9, false),
        );
        let mut different_compatible_id = candidate("oem7.inf", "pci\\ven_8086&dev_2822", 9, false);
        different_compatible_id.compatible_ids = "PCI\\CC_0106".into();
        push_unique_candidate(&mut candidates, &mut seen, different_compatible_id);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn cleanup_failure_is_preserved_with_primary_failure() {
        let result: anyhow::Result<()> = combine_primary_and_cleanup(
            Err(anyhow::anyhow!("primary failure")),
            vec!["close failure".into(), "shutdown failure".into()],
        );
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("primary failure"));
        assert!(message.contains("close failure"));
        assert!(message.contains("shutdown failure"));
    }

    #[test]
    fn cleanup_failure_turns_success_into_error() {
        let result = combine_primary_and_cleanup(Ok(7_u32), vec!["delete failure".into()]);
        assert!(format!("{:#}", result.unwrap_err()).contains("delete failure"));
    }

    #[test]
    fn unrelated_package_failure_preserves_existing_coverage() {
        let mut inventory = OfflineDriverInventory {
            candidates: vec![candidate("storahci.inf", "PCI\\CC_0106", 9, true)],
            ..OfflineDriverInventory::default()
        };
        record_package_query_failure(
            &mut inventory.package_query_failures,
            &mut inventory.omitted_package_query_failures,
            OfflineDriverPackageQueryFailure {
                published_name: "unrelated-printer.inf".into(),
                hresult: 0x8007_0002,
                detail: "package query failed".into(),
            },
        );
        assert!(
            find_offline_driver_coverage(&inventory.candidates, &["pci\\cc_0106".into()]).is_some()
        );
        assert_eq!(inventory.package_query_failures.len(), 1);
    }

    #[test]
    fn package_failures_and_details_are_bounded() {
        let mut failures = Vec::new();
        let mut omitted = 0;
        for index in 0..(MAX_RECORDED_PACKAGE_FAILURES + 3) {
            record_package_query_failure(
                &mut failures,
                &mut omitted,
                OfflineDriverPackageQueryFailure {
                    published_name: format!("oem{index}.inf"),
                    hresult: 0x8000_4005,
                    detail: "failure".into(),
                },
            );
        }
        assert_eq!(failures.len(), MAX_RECORDED_PACKAGE_FAILURES);
        assert_eq!(omitted, 3);

        let bounded = bound_detail("x".repeat(MAX_PACKAGE_FAILURE_DETAIL_CHARS + 10));
        assert_eq!(bounded.chars().count(), MAX_PACKAGE_FAILURE_DETAIL_CHARS);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn parses_invariant_english_driver_package_list_without_localized_fields() {
        let output = r#"
Deployment Image Servicing and Management tool

Published Name : storvsc.inf
Original File Name : storvsc.inf
Inbox : Yes
Class Name : SCSIAdapter
Class Description : Storage controllers

Published Name : oem7.inf
Original File Name : vendor.inf
Inbox : No
Class Name : HDC
Provider Name : Vendor

The operation completed successfully.
"#;
        let packages = parse_dism_get_drivers_english(output).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].published_name, "storvsc.inf");
        assert_eq!(packages[0].class_name, "SCSIAdapter");
        assert!(packages[0].in_box);
        assert_eq!(packages[1].original_file_name, "vendor.inf");
        assert!(!packages[1].in_box);
    }

    #[test]
    fn driver_package_list_rejects_localized_or_duplicate_out_of_box_identity_records() {
        let localized = r#"
发布名称 : storvsc.inf
原始文件名 : storvsc.inf
收件箱 : 是
类名 : SCSIAdapter
"#;
        assert!(parse_dism_get_drivers_english(localized).is_err());

        let duplicate = r#"
Published Name : oem1.inf
Original File Name : first.inf
Inbox : No
Class Name : HDC
Published Name : OEM1.INF
Original File Name : second.inf
Inbox : No
Class Name : HDC
"#;
        assert!(parse_dism_get_drivers_english(duplicate).is_err());
    }

    #[test]
    fn repeated_inbox_published_name_is_queried_once() {
        // Serviced Windows 10 images can list the same inbox published name more than once.
        // The published name is still the documented input to the authoritative detail query.
        let packages = parse_dism_get_drivers_english(
            r#"
Published Name : ntprint.inf
Original File Name : ntprint.inf
Inbox : Yes
Class Name : Printer

Published Name : NTPRINT.INF
Original File Name : ntprint.inf
Inbox : Yes
Class Name : Printer
"#,
        )
        .unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].published_name, "ntprint.inf");
        assert!(packages[0].in_box);
    }

    #[test]
    fn repeated_name_mixing_inbox_and_out_of_box_is_rejected() {
        let output = r#"
Published Name : duplicate.inf
Original File Name : duplicate.inf
Inbox : Yes
Class Name : System

Published Name : DUPLICATE.INF
Original File Name : vendor.inf
Inbox : No
Class Name : System
"#;
        assert!(parse_dism_get_drivers_english(output).is_err());
    }

    #[test]
    fn parses_only_image_applicable_hardware_ids_from_driver_info() {
        let package = parse_dism_get_drivers_english(
            r#"
Published Name : storvsc.inf
Original File Name : storvsc.inf
Inbox : Yes
Class Name : SCSIAdapter
"#,
        )
        .unwrap()
        .remove(0);
        let candidates = parse_dism_get_driver_info_english(
            r#"
Published Name : storvsc.inf
Boot Critical : Yes

Drivers for architecture : amd64
Manufacturer : Microsoft
Description : Microsoft Hyper-V SCSI Controller
Architecture : x64
Hardware ID : VMBUS\{BA6163D9-04A1-4D29-B605-72E2FFB1DC7F}
Service Name : storvsc
Compatible IDs : VMBUS\{BA6163D9-04A1-4D29-B605-72E2FFB1DC70}

Manufacturer : Microsoft
Description : Microsoft Hyper-V SCSI Controller
Architecture : amd64
Hardware ID :
Service Name : storvsc
Compatible IDs : VMBUS\{32412632-86CB-44A2-9B5C-50D1417354F5}

The operation completed successfully.
"#,
            &package,
        )
        .unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| candidate.in_box));
        assert!(candidates
            .iter()
            .all(|candidate| candidate.boot_critical && candidate.architecture == 9));
        assert_eq!(
            candidates[0].hardware_id,
            r"VMBUS\{BA6163D9-04A1-4D29-B605-72E2FFB1DC7F}"
        );
        assert_eq!(
            candidates[0].compatible_ids,
            r"VMBUS\{BA6163D9-04A1-4D29-B605-72E2FFB1DC70}"
        );
        assert!(candidates[1].hardware_id.is_empty());
        assert_eq!(
            candidates[1].compatible_ids,
            r"VMBUS\{32412632-86CB-44A2-9B5C-50D1417354F5}"
        );
    }

    #[test]
    fn driver_info_requires_matching_published_name_and_architecture_before_id() {
        let package = parse_dism_get_drivers_english(
            r#"
Published Name : storahci.inf
Original File Name : storahci.inf
Inbox : Yes
Class Name : HDC
"#,
        )
        .unwrap()
        .remove(0);
        assert!(parse_dism_get_driver_info_english(
            "Published Name : other.inf\nArchitecture : x64\nHardware ID : PCI\\CC_0106\n",
            &package,
        )
        .is_err());
        assert!(parse_dism_get_driver_info_english(
            "Published Name : storahci.inf\nHardware ID : PCI\\CC_0106\n",
            &package,
        )
        .is_err());
        assert!(parse_dism_get_driver_info_english(
            "Published Name : storahci.inf\nCompatible IDs : PCI\\CC_0106\n",
            &package,
        )
        .is_err());
    }

    #[test]
    fn parses_repeated_driver_detail_reports_and_rejects_missing_reports() {
        let packages = parse_dism_get_drivers_english(
            r#"
Published Name : storahci.inf
Original File Name : storahci.inf
Inbox : Yes
Class Name : HDC
Published Name : oem2.inf
Original File Name : vendor.inf
Inbox : No
Class Name : SCSIAdapter
"#,
        )
        .unwrap();
        let output = r#"
Published Name : storahci.inf
Boot Critical : Yes
Architecture : amd64
Hardware ID : PCI\CC_0106

Published Name : oem2.inf
Boot Critical : No
Architecture : x64
Hardware ID : PCI\VEN_1234&DEV_5678
The operation completed successfully.
"#;
        let candidates = parse_dism_get_driver_infos_english(output, &packages).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].published_name, "storahci.inf");
        assert_eq!(candidates[1].published_name, "oem2.inf");

        assert!(parse_dism_get_driver_infos_english(
            "Published Name : storahci.inf\nArchitecture : x64\nHardware ID : PCI\\CC_0106\n",
            &packages,
        )
        .is_err());
    }
}
