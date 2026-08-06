//! Independent Windows driver-package trust verification used by the narrow DISM fallback.
//!
//! DISM can occasionally reject a boot-critical package as unsigned even though SetupAPI
//! accepts the package catalog.  This module verifies one concrete INF through SetupAPI and
//! binds that decision to hashes of the INF and catalog.  It deliberately does not expose a
//! generic "allow unsigned" switch.

use std::ffi::c_void;
use std::ffi::OsStr;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use windows::core::{GUID, PCSTR, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupVerifyInfFileW, SP_ALTPLATFORM_INFO_V2, SP_INF_SIGNER_INFO_V2_W,
};
use windows::Win32::Foundation::{GetLastError, BOOL, HANDLE};
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, DRIVER_ACTION_VERIFY, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO,
    WINTRUST_DATA, WINTRUST_DATA_0, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_CATALOG,
    WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use windows::Win32::System::Diagnostics::Debug::VER_PLATFORM_WIN32_NT;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::{
    PROCESSOR_ARCHITECTURE, PROCESSOR_ARCHITECTURE_AMD64, PROCESSOR_ARCHITECTURE_INTEL,
};

/// An authorization for one exact, SetupAPI-verified INF package.
///
/// Fields stay private so callers cannot manufacture permission to use `/ForceUnsigned`.
#[derive(Debug, Clone)]
pub struct VerifiedDriverPackage {
    inf_path: PathBuf,
    catalog_path: PathBuf,
    package_files: Vec<PackageFileFingerprint>,
    signer: String,
}

/// Result of auditing a staged driver tree without treating an unrelated optional package as a
/// fatal installation error. Structural traversal errors remain fatal; individual package trust
/// failures are returned to the caller for logging and later exact-INF handling.
#[derive(Debug, Clone)]
pub struct DriverDirectoryTrustReport {
    total: usize,
    verified: usize,
    rejected: Vec<DriverPackageTrustFailure>,
}

#[derive(Debug, Clone)]
pub struct DriverPackageTrustFailure {
    inf_path: PathBuf,
    reason: String,
}

impl DriverDirectoryTrustReport {
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn verified(&self) -> usize {
        self.verified
    }

    pub fn rejected(&self) -> &[DriverPackageTrustFailure] {
        &self.rejected
    }
}

impl DriverPackageTrustFailure {
    pub fn inf_path(&self) -> &Path {
        &self.inf_path
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageFileFingerprint {
    relative_path: PathBuf,
    size: u64,
    sha256: String,
}

impl VerifiedDriverPackage {
    pub fn inf_path(&self) -> &Path {
        &self.inf_path
    }

    pub fn catalog_path(&self) -> &Path {
        &self.catalog_path
    }

    pub fn signer(&self) -> &str {
        &self.signer
    }

    /// Rechecks the path and hashes immediately before the privileged DISM retry.
    pub fn revalidate(&self) -> Result<()> {
        validate_regular_inf(&self.inf_path)?;
        validate_regular_file(&self.catalog_path, "driver catalog")?;
        let package_dir = self
            .inf_path
            .parent()
            .context("verified INF has no package directory")?;
        if snapshot_package_files(package_dir)? != self.package_files {
            bail!("driver package changed after signature verification");
        }
        Ok(())
    }
}

/// Verifies one INF with the Windows SetupAPI catalog policy and returns a hash-bound token.
pub fn verify_driver_package(inf_path: &Path) -> Result<VerifiedDriverPackage> {
    validate_regular_inf(inf_path)?;
    let absolute_inf = inf_path
        .canonicalize()
        .with_context(|| format!("failed to resolve INF: {}", inf_path.display()))?;
    let wide = wide_null(absolute_inf.as_os_str());
    let mut attempts = vec![None];
    for &(major, minor) in &[(10, 0), (6, 3), (6, 2), (6, 1)] {
        attempts.push(Some(alternate_platform(
            major,
            minor,
            PROCESSOR_ARCHITECTURE_AMD64,
        )));
        attempts.push(Some(alternate_platform(
            major,
            minor,
            PROCESSOR_ARCHITECTURE_INTEL,
        )));
    }

    let mut accepted_signer = None;
    let mut last_error = 0;
    for platform in &attempts {
        let mut signer = SP_INF_SIGNER_INFO_V2_W {
            cbSize: size_of::<SP_INF_SIGNER_INFO_V2_W>() as u32,
            ..Default::default()
        };
        let verified = unsafe {
            SetupVerifyInfFileW(
                PCWSTR(wide.as_ptr()),
                platform.as_ref().map(|value| value as *const _),
                &mut signer,
            )
        };
        if verified.as_bool() {
            accepted_signer = Some(signer);
            break;
        }
        last_error = unsafe { GetLastError() }.0;
    }
    let signer = accepted_signer.ok_or_else(|| {
        anyhow::anyhow!(
            "SetupVerifyInfFileW rejected {} for all supported Windows x86/x64 targets (Win32 error {})",
            absolute_inf.display(),
            last_error
        )
    })?;

    let signer_name = wide_array_to_string(&signer.DigitalSigner);
    let catalog_name = wide_array_to_string(&signer.CatalogFile);
    if signer_name.trim().is_empty() {
        bail!("SetupAPI returned an empty driver signer");
    }
    if catalog_name.trim().is_empty() {
        bail!("SetupAPI returned an empty driver catalog path");
    }

    let reported_catalog = PathBuf::from(catalog_name);
    let catalog_path = if reported_catalog.is_absolute() {
        reported_catalog
    } else {
        absolute_inf
            .parent()
            .context("verified INF has no parent directory")?
            .join(reported_catalog)
    };
    validate_regular_file(&catalog_path, "driver catalog")?;
    let absolute_catalog = catalog_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve driver catalog: {}",
            catalog_path.display()
        )
    })?;

    // SetupAPI must not be allowed to authorize a catalog outside this concrete package.
    let package_dir = absolute_inf
        .parent()
        .context("verified INF has no package directory")?;
    if absolute_catalog.parent() != Some(package_dir) {
        bail!(
            "verified driver catalog is outside the INF package directory: {}",
            absolute_catalog.display()
        );
    }

    let package_files = snapshot_package_files(package_dir)?;
    verify_catalog_payload_members(package_dir, &absolute_catalog, &package_files)?;

    Ok(VerifiedDriverPackage {
        inf_path: absolute_inf,
        catalog_path: absolute_catalog,
        package_files,
        signer: signer_name,
    })
}

/// Verifies executable and firmware payloads as concrete members of the catalog returned by
/// SetupAPI. Extra vendor documentation can legitimately be shipped beside a driver package, so
/// it remains hash-bound for TOCTOU protection without being required to be a catalog member.
fn verify_catalog_payload_members(
    package_dir: &Path,
    catalog_path: &Path,
    files: &[PackageFileFingerprint],
) -> Result<()> {
    let mut verified = 0usize;
    for file in files {
        let extension = file
            .relative_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !is_catalog_payload_extension(extension) {
            continue;
        }
        let member_path = package_dir.join(&file.relative_path);
        verify_catalog_member(catalog_path, &member_path).with_context(|| {
            format!(
                "driver payload is not a trusted member of {}: {}",
                catalog_path.display(),
                member_path.display()
            )
        })?;
        verified += 1;
    }
    if verified == 0 {
        bail!("driver catalog verification found no INF or executable payload members");
    }
    Ok(())
}

fn is_catalog_payload_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "inf"
            | "sys"
            | "dll"
            | "exe"
            | "efi"
            | "drv"
            | "ocx"
            | "cpl"
            | "ax"
            | "bin"
            | "fw"
            | "rom"
            | "hex"
            | "dat"
    )
}

type CryptCatAdminAcquireContext2 =
    unsafe extern "system" fn(*mut isize, *const GUID, *const u16, *const c_void, u32) -> BOOL;
type CryptCatAdminCalcHashFromFileHandle2 =
    unsafe extern "system" fn(isize, HANDLE, *mut u32, *mut u8, u32) -> BOOL;
type CryptCatAdminAcquireContext = unsafe extern "system" fn(*mut isize, *const GUID, u32) -> BOOL;
type CryptCatAdminCalcHashFromFileHandle =
    unsafe extern "system" fn(HANDLE, *mut u32, *mut u8, u32) -> BOOL;
type CryptCatAdminReleaseContext = unsafe extern "system" fn(isize, u32) -> BOOL;

#[derive(Clone, Copy)]
enum CatalogHashApi {
    Modern {
        acquire_context: CryptCatAdminAcquireContext2,
        calc_hash: CryptCatAdminCalcHashFromFileHandle2,
    },
    Windows7 {
        acquire_context: CryptCatAdminAcquireContext,
        calc_hash: CryptCatAdminCalcHashFromFileHandle,
    },
}

struct CatalogApi {
    hash: CatalogHashApi,
    release_context: CryptCatAdminReleaseContext,
}

impl CatalogApi {
    fn load() -> Result<Self> {
        // WinVerifyTrust is statically imported above, so this obtains the already loaded system
        // Wintrust.dll and cannot be redirected to a driver-package directory.
        let module_name = wide_null(OsStr::new("wintrust.dll"));
        let module = unsafe { GetModuleHandleW(PCWSTR(module_name.as_ptr())) }
            .context("Wintrust.dll is not loaded")?;
        let acquire_context2 = load_catalog_proc(module, b"CryptCATAdminAcquireContext2\0");
        let calc_hash2 = load_catalog_proc(module, b"CryptCATAdminCalcHashFromFileHandle2\0");
        let hash = match (acquire_context2, calc_hash2) {
            (Ok(acquire_context), Ok(calc_hash)) => CatalogHashApi::Modern {
                acquire_context,
                calc_hash,
            },
            // Microsoft documents the *2 pair as Windows 8+. Keep it on every modern system,
            // while Windows 7 falls back to the original XP-era pair. Mixing one generation's
            // context with the other generation's hash function is deliberately forbidden.
            _ => CatalogHashApi::Windows7 {
                acquire_context: load_catalog_proc(module, b"CryptCATAdminAcquireContext\0")?,
                calc_hash: load_catalog_proc(module, b"CryptCATAdminCalcHashFromFileHandle\0")?,
            },
        };
        Ok(Self {
            hash,
            release_context: load_catalog_proc(module, b"CryptCATAdminReleaseContext\0")?,
        })
    }
}

fn load_catalog_proc<T: Copy>(
    module: windows::Win32::Foundation::HMODULE,
    name: &'static [u8],
) -> Result<T> {
    let procedure = unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }.ok_or_else(|| {
        anyhow::anyhow!("Wintrust.dll is missing {}", String::from_utf8_lossy(name))
    })?;
    // Every requested symbol is immediately transmuted to its documented system ABI signature.
    Ok(unsafe { std::mem::transmute_copy(&procedure) })
}

struct CatalogContext<'a> {
    api: &'a CatalogApi,
    handle: isize,
}

impl Drop for CatalogContext<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.api.release_context)(self.handle, 0);
        }
    }
}

fn verify_catalog_member(catalog_path: &Path, member_path: &Path) -> Result<()> {
    validate_regular_file(member_path, "driver payload")?;
    let api = CatalogApi::load()?;
    let file = std::fs::File::open(member_path)
        .with_context(|| format!("failed to open driver payload: {}", member_path.display()))?;
    let member_handle = HANDLE(file.as_raw_handle());

    let mut failures = Vec::new();
    for algorithm in ["SHA256", "SHA1"] {
        match verify_catalog_member_with_algorithm(
            &api,
            catalog_path,
            member_path,
            member_handle,
            algorithm,
        ) {
            Ok(()) => return Ok(()),
            Err(error) => failures.push(format!("{algorithm}: {error:#}")),
        }
    }
    bail!(
        "catalog member verification failed for every supported hash policy: {}",
        failures.join("; ")
    )
}

fn verify_catalog_member_with_algorithm(
    api: &CatalogApi,
    catalog_path: &Path,
    member_path: &Path,
    member_handle: HANDLE,
    algorithm: &str,
) -> Result<()> {
    let api_generation = match api.hash {
        CatalogHashApi::Modern { .. } => "modern",
        CatalogHashApi::Windows7 { .. } => "Windows 7 compatible",
    };
    let mut context_handle = 0isize;
    let acquired = match api.hash {
        CatalogHashApi::Modern {
            acquire_context, ..
        } => {
            let algorithm_wide = wide_null(OsStr::new(algorithm));
            unsafe {
                acquire_context(
                    &mut context_handle,
                    &DRIVER_ACTION_VERIFY,
                    algorithm_wide.as_ptr(),
                    std::ptr::null(),
                    0,
                )
            }
        }
        CatalogHashApi::Windows7 {
            acquire_context, ..
        } => {
            if algorithm != "SHA1" {
                bail!(
                    "the Windows 7 catalog API cannot select the requested {algorithm} hash policy"
                );
            }
            unsafe { acquire_context(&mut context_handle, &DRIVER_ACTION_VERIFY, 0) }
        }
    };
    if !acquired.as_bool() || context_handle == 0 {
        bail!(
            "{api_generation} catalog context acquisition failed with Win32 error {}",
            unsafe { GetLastError() }.0
        );
    }
    let context = CatalogContext {
        api,
        handle: context_handle,
    };

    let mut hash_size = 0u32;
    let measured = unsafe {
        match api.hash {
            CatalogHashApi::Modern { calc_hash, .. } => calc_hash(
                context.handle,
                member_handle,
                &mut hash_size,
                std::ptr::null_mut(),
                0,
            ),
            CatalogHashApi::Windows7 { calc_hash, .. } => {
                calc_hash(member_handle, &mut hash_size, std::ptr::null_mut(), 0)
            }
        }
    };
    if !measured.as_bool() || hash_size == 0 || hash_size > 128 {
        bail!(
            "{api_generation} catalog hash size query failed (size {}, Win32 error {})",
            hash_size,
            unsafe { GetLastError() }.0
        );
    }
    let mut hash = vec![0u8; hash_size as usize];
    let calculated = unsafe {
        match api.hash {
            CatalogHashApi::Modern { calc_hash, .. } => calc_hash(
                context.handle,
                member_handle,
                &mut hash_size,
                hash.as_mut_ptr(),
                0,
            ),
            CatalogHashApi::Windows7 { calc_hash, .. } => {
                calc_hash(member_handle, &mut hash_size, hash.as_mut_ptr(), 0)
            }
        }
    };
    if !calculated.as_bool() || hash_size as usize != hash.len() {
        bail!(
            "{api_generation} catalog hash calculation failed (size {}, Win32 error {})",
            hash_size,
            unsafe { GetLastError() }.0
        );
    }

    let member_tag = hash
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    let catalog_wide = wide_null(catalog_path.as_os_str());
    let member_path_wide = wide_null(member_path.as_os_str());
    let member_tag_wide = wide_null(OsStr::new(&member_tag));
    let mut catalog_info = WINTRUST_CATALOG_INFO {
        cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
        pcwszCatalogFilePath: PCWSTR(catalog_wide.as_ptr()),
        pcwszMemberTag: PCWSTR(member_tag_wide.as_ptr()),
        pcwszMemberFilePath: PCWSTR(member_path_wide.as_ptr()),
        hMemberFile: member_handle,
        pbCalculatedFileHash: hash.as_mut_ptr(),
        cbCalculatedFileHash: hash.len() as u32,
        hCatAdmin: context.handle,
        ..Default::default()
    };
    let mut trust_data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_CATALOG,
        Anonymous: WINTRUST_DATA_0 {
            pCatalog: &mut catalog_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status =
        unsafe { WinVerifyTrust(None, &mut action, &mut trust_data as *mut _ as *mut c_void) };
    trust_data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(None, &mut action, &mut trust_data as *mut _ as *mut c_void);
    }
    if status != 0 {
        bail!(
            "WinVerifyTrust catalog policy returned 0x{:08X}",
            status as u32
        );
    }
    Ok(())
}

fn enumerate_driver_infs(driver_dir: &Path) -> Result<Vec<PathBuf>> {
    let metadata = driver_dir
        .symlink_metadata()
        .with_context(|| format!("driver directory is unavailable: {}", driver_dir.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "driver source is not a regular directory: {}",
            driver_dir.display()
        );
    }

    let mut inf_files = Vec::new();
    for entry in walkdir::WalkDir::new(driver_dir).follow_links(false) {
        let entry = entry.with_context(|| {
            format!(
                "failed to enumerate driver directory: {}",
                driver_dir.display()
            )
        })?;
        if entry.file_type().is_symlink() {
            bail!(
                "driver tree contains a reparse entry: {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case("inf"))
                == Some(true)
        {
            inf_files.push(entry.path().to_path_buf());
        }
    }
    inf_files.sort();
    Ok(inf_files)
}

/// Audits every INF package while keeping package-level failures separate from unsafe directory
/// structure or enumeration failures.
pub fn audit_driver_directory(driver_dir: &Path) -> Result<DriverDirectoryTrustReport> {
    let inf_files = enumerate_driver_infs(driver_dir)?;
    let mut verified = 0;
    let mut rejected = Vec::new();
    for inf in &inf_files {
        match verify_driver_package(inf) {
            Ok(_) => verified += 1,
            Err(error) => rejected.push(DriverPackageTrustFailure {
                inf_path: inf.clone(),
                reason: format!("{error:#}"),
            }),
        }
    }
    Ok(DriverDirectoryTrustReport {
        total: inf_files.len(),
        verified,
        rejected,
    })
}

/// Strict compatibility wrapper for callers that explicitly require every package to be trusted.
pub fn verify_driver_directory(driver_dir: &Path) -> Result<usize> {
    let report = audit_driver_directory(driver_dir)?;
    if let Some(failure) = report.rejected().first() {
        bail!(
            "driver preflight failed for {}: {}",
            failure.inf_path().display(),
            failure.reason()
        );
    }
    Ok(report.verified())
}

/// Only the DISM failure shape observed for signed boot-critical packages is eligible.
pub fn is_known_dism_signature_false_negative(error: &str) -> bool {
    let text = error.to_ascii_lowercase();
    let signature_signal = text.contains("unsigned")
        || text.contains("not digitally signed")
        || text.contains("signature")
        || error.contains("未签名")
        || error.contains("签名");
    let known_code = text.contains("0x80070032")
        || text.contains("error: 50")
        || text.contains("error 50")
        || text.contains("错误: 50")
        || text.contains("错误 50");
    signature_signal || known_code
}

fn validate_regular_inf(path: &Path) -> Result<()> {
    if path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("inf"))
        != Some(true)
    {
        bail!("controlled driver fallback requires one concrete INF file");
    }
    validate_regular_file(path, "driver INF")
}

fn validate_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = path
        .symlink_metadata()
        .with_context(|| format!("{} is unavailable: {}", label, path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("{} is not a regular file: {}", label, path.display());
    }
    Ok(())
}

fn snapshot_package_files(package_dir: &Path) -> Result<Vec<PackageFileFingerprint>> {
    const MAX_PACKAGE_FILES: usize = 4096;
    const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

    let mut files = Vec::new();
    let mut total_size = 0u64;
    for entry in walkdir::WalkDir::new(package_dir).follow_links(false) {
        let entry = entry.with_context(|| {
            format!(
                "failed to enumerate driver package directory: {}",
                package_dir.display()
            )
        })?;
        if entry.file_type().is_symlink() {
            bail!(
                "driver package contains a reparse entry: {}",
                entry.path().display()
            );
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if files.len() >= MAX_PACKAGE_FILES {
            bail!("driver package contains too many files");
        }
        let metadata = entry.metadata().with_context(|| {
            format!(
                "failed to read driver package file: {}",
                entry.path().display()
            )
        })?;
        total_size = total_size
            .checked_add(metadata.len())
            .context("driver package size overflow")?;
        if total_size > MAX_PACKAGE_BYTES {
            bail!("driver package exceeds the verification size limit");
        }
        let relative_path = entry
            .path()
            .strip_prefix(package_dir)
            .context("driver package path escaped its root")?
            .to_path_buf();
        let sha256 = crate::hash::sha256_file(entry.path(), |_| {}).with_context(|| {
            format!(
                "failed to hash driver package file: {}",
                entry.path().display()
            )
        })?;
        files.push(PackageFileFingerprint {
            relative_path,
            size: metadata.len(),
            sha256,
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if files.is_empty() {
        bail!("driver package directory is empty");
    }
    Ok(files)
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn alternate_platform(
    major: u32,
    minor: u32,
    architecture: PROCESSOR_ARCHITECTURE,
) -> SP_ALTPLATFORM_INFO_V2 {
    SP_ALTPLATFORM_INFO_V2 {
        cbSize: size_of::<SP_ALTPLATFORM_INFO_V2>() as u32,
        Platform: VER_PLATFORM_WIN32_NT,
        MajorVersion: major,
        MinorVersion: minor,
        ProcessorArchitecture: architecture,
        Anonymous: Default::default(),
        FirstValidatedMajorVersion: 0,
        FirstValidatedMinorVersion: 0,
    }
}

fn wide_array_to_string(value: &[u16]) -> String {
    let end = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn fallback_classifier_accepts_the_observed_boot_driver_failure() {
        assert!(is_known_dism_signature_false_negative(
            "Error: 50 driver package is unsigned (0x80070032)"
        ));
        assert!(!is_known_dism_signature_false_negative(
            "The system cannot find the path specified"
        ));
    }

    #[test]
    fn directory_cannot_be_verified_as_a_driver_package() {
        let error = verify_driver_package(Path::new(".")).unwrap_err();
        assert!(error.to_string().contains("concrete INF"));
    }

    #[test]
    fn directory_audit_reports_a_bad_optional_package_without_failing_traversal() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lr-driver-directory-audit-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let inf = root.join("optional.inf");
        std::fs::write(&inf, b"not a signed driver package").unwrap();

        let report = audit_driver_directory(&root).unwrap();
        assert_eq!(report.total(), 1);
        assert_eq!(report.verified(), 0);
        assert_eq!(report.rejected().len(), 1);
        assert_eq!(report.rejected()[0].inf_path(), inf);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authorization_detects_payload_changes_not_only_inf_or_catalog_changes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lr-driver-package-trust-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let inf = root.join("sample.inf");
        let catalog = root.join("sample.cat");
        let payload = root.join("sample.sys");
        std::fs::write(&inf, b"inf").unwrap();
        std::fs::write(&catalog, b"catalog").unwrap();
        std::fs::write(&payload, b"original payload").unwrap();

        let package = VerifiedDriverPackage {
            inf_path: inf,
            catalog_path: catalog,
            package_files: snapshot_package_files(&root).unwrap(),
            signer: "test signer".into(),
        };
        std::fs::write(&payload, b"changed payload").unwrap();
        let error = package.revalidate().unwrap_err();
        assert!(error
            .to_string()
            .contains("changed after signature verification"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires LR_TEST_SIGNED_DRIVER_INF to point to a real signed package"]
    fn verifies_real_package_selected_by_integration_environment() {
        let inf = std::env::var_os("LR_TEST_SIGNED_DRIVER_INF")
            .expect("LR_TEST_SIGNED_DRIVER_INF is required");
        let verified = verify_driver_package(Path::new(&inf)).unwrap();
        verified.revalidate().unwrap();
        assert!(!verified.signer().is_empty());
    }

    #[test]
    #[ignore = "requires LR_TEST_SIGNED_DRIVER_INF to point to a real signed package"]
    fn rejects_a_mutated_payload_from_a_real_signed_package() {
        let source_inf = PathBuf::from(
            std::env::var_os("LR_TEST_SIGNED_DRIVER_INF")
                .expect("LR_TEST_SIGNED_DRIVER_INF is required"),
        );
        let source_dir = source_inf.parent().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lr-driver-catalog-mutation-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for entry in walkdir::WalkDir::new(source_dir) {
            let entry = entry.unwrap();
            let relative = entry.path().strip_prefix(source_dir).unwrap();
            let destination = root.join(relative);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&destination).unwrap();
            } else if entry.file_type().is_file() {
                std::fs::copy(entry.path(), destination).unwrap();
            }
        }
        let copied_inf = root.join(source_inf.file_name().unwrap());
        verify_driver_package(&copied_inf).unwrap();
        let payload = walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .find(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(|value| value.eq_ignore_ascii_case("sys"))
                        == Some(true)
            })
            .expect("integration package must contain a SYS payload")
            .into_path();
        let mut bytes = std::fs::read(&payload).unwrap();
        bytes[0] ^= 0x01;
        std::fs::write(&payload, bytes).unwrap();
        let error = verify_driver_package(&copied_inf).unwrap_err();
        assert!(error.to_string().contains("trusted member"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
