//! Supported offline removal for a deliberately narrow set of provisioned AppX packages.
//!
//! Package names always come from a fresh DISM inventory. A package is eligible only when the
//! documented package identity APIs independently confirm both its exact family name and package
//! identity name. On a Windows 7 host, where those Windows 8 package-identity exports do not
//! exist, a strict parser for Microsoft's documented package-full-name shape derives the same
//! name and family; malformed or non-canonical inventory remains fail-closed. The first-boot
//! retry uses exact PowerShell AppX objects and narrow retirement markers, never wildcard package
//! or WindowsApps-directory deletion.

use crate::command::{CommandExecutor, CommandOutcome, CommandRequest, SystemCommandExecutor};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result as AnyResult};

pub const CURATED_ONLINE_SCRIPT_FILE_NAME: &str = "remove-curated-appx.ps1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CuratedAppxIdentity {
    pub id: &'static str,
    pub package_name: &'static str,
    pub package_family_name: &'static str,
}

pub const CURATED_PREINSTALLED_APPX: &[CuratedAppxIdentity] = &[
    CuratedAppxIdentity {
        id: "clipchamp",
        package_name: "Clipchamp.Clipchamp",
        package_family_name: "Clipchamp.Clipchamp_yxz26nhyzhsrt",
    },
    CuratedAppxIdentity {
        id: "news",
        package_name: "Microsoft.BingNews",
        package_family_name: "Microsoft.BingNews_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "weather",
        package_name: "Microsoft.BingWeather",
        package_family_name: "Microsoft.BingWeather_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "microsoft_365",
        package_name: "Microsoft.MicrosoftOfficeHub",
        package_family_name: "Microsoft.MicrosoftOfficeHub_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "microsoft_pc_manager",
        package_name: "Microsoft.MicrosoftPCManager",
        package_family_name: "Microsoft.MicrosoftPCManager_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "sticky_notes",
        package_name: "Microsoft.MicrosoftStickyNotes",
        package_family_name: "Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "family",
        package_name: "MicrosoftCorporationII.MicrosoftFamily",
        package_family_name: "MicrosoftCorporationII.MicrosoftFamily_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "teams_new",
        package_name: "MSTeams",
        package_family_name: "MSTeams_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "teams_classic",
        package_name: "MicrosoftTeams",
        package_family_name: "MicrosoftTeams_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "microsoft_todo",
        package_name: "Microsoft.Todos",
        package_family_name: "Microsoft.Todos_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "solitaire",
        package_name: "Microsoft.MicrosoftSolitaireCollection",
        package_family_name: "Microsoft.MicrosoftSolitaireCollection_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "get_started",
        package_name: "Microsoft.Getstarted",
        package_family_name: "Microsoft.Getstarted_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "clock",
        package_name: "Microsoft.WindowsAlarms",
        package_family_name: "Microsoft.WindowsAlarms_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "sound_recorder",
        package_name: "Microsoft.WindowsSoundRecorder",
        package_family_name: "Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "phone_link",
        package_name: "Microsoft.YourPhone",
        package_family_name: "Microsoft.YourPhone_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "quick_assist",
        package_name: "MicrosoftCorporationII.QuickAssist",
        package_family_name: "MicrosoftCorporationII.QuickAssist_8wekyb3d8bbwe",
    },
];

const CURATED_ONLINE_REMOVAL_SCRIPT: &str = r#"[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$allowed = @(
  @{ Name='Clipchamp.Clipchamp'; Family='Clipchamp.Clipchamp_yxz26nhyzhsrt' },
  @{ Name='Microsoft.BingNews'; Family='Microsoft.BingNews_8wekyb3d8bbwe' },
  @{ Name='Microsoft.BingWeather'; Family='Microsoft.BingWeather_8wekyb3d8bbwe' },
  @{ Name='Microsoft.Getstarted'; Family='Microsoft.Getstarted_8wekyb3d8bbwe' },
  @{ Name='Microsoft.MicrosoftOfficeHub'; Family='Microsoft.MicrosoftOfficeHub_8wekyb3d8bbwe' },
  @{ Name='Microsoft.MicrosoftPCManager'; Family='Microsoft.MicrosoftPCManager_8wekyb3d8bbwe' },
  @{ Name='Microsoft.MicrosoftSolitaireCollection'; Family='Microsoft.MicrosoftSolitaireCollection_8wekyb3d8bbwe' },
  @{ Name='Microsoft.MicrosoftStickyNotes'; Family='Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe' },
  @{ Name='Microsoft.Todos'; Family='Microsoft.Todos_8wekyb3d8bbwe' },
  @{ Name='Microsoft.WindowsAlarms'; Family='Microsoft.WindowsAlarms_8wekyb3d8bbwe' },
  @{ Name='Microsoft.WindowsSoundRecorder'; Family='Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe' },
  @{ Name='Microsoft.YourPhone'; Family='Microsoft.YourPhone_8wekyb3d8bbwe' },
  @{ Name='MicrosoftCorporationII.MicrosoftFamily'; Family='MicrosoftCorporationII.MicrosoftFamily_8wekyb3d8bbwe' },
  @{ Name='MicrosoftCorporationII.QuickAssist'; Family='MicrosoftCorporationII.QuickAssist_8wekyb3d8bbwe' },
  @{ Name='MicrosoftTeams'; Family='MicrosoftTeams_8wekyb3d8bbwe' },
  @{ Name='MSTeams'; Family='MSTeams_8wekyb3d8bbwe' }
)
$failures = [System.Collections.Generic.List[string]]::new()
$diagnostics = [System.Collections.Generic.List[string]]::new()
$logDirectory = [System.IO.Path]::Combine($env:ProgramData, 'LetRecovery', 'Logs')

function Test-LetRecoveryAppxToken([string]$value) {
  return (-not [string]::IsNullOrWhiteSpace($value)) -and
    $value.Length -le 512 -and
    $value -match '\A[A-Za-z0-9._~-]+\z'
}

function Import-LetRecoveryAppxRetirementMarkers([string]$family, [object[]]$packages) {
  if (-not (Test-LetRecoveryAppxToken $family)) { throw ('unsafe package family name: {0}' -f $family) }
  [void][System.IO.Directory]::CreateDirectory($logDirectory)
  $directoryInfo = Get-Item -LiteralPath $logDirectory -Force -ErrorAction Stop
  if (-not $directoryInfo.PSIsContainer -or (($directoryInfo.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
    throw 'retirement marker directory is not a regular directory'
  }
  $keys = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
  [void]$keys.Add(('HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Appx\AppxAllUserStore\Deprovisioned\{0}' -f $family))
  foreach ($package in $packages) {
    $fullName = [string]$package.PackageFullName
    if ([string]::IsNullOrWhiteSpace($fullName)) { $fullName = [string]$package.PackageName }
    if (-not (Test-LetRecoveryAppxToken $fullName)) { throw ('unsafe package full name: {0}' -f $fullName) }
    $sids = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    [void]$sids.Add('S-1-5-18')
    foreach ($user in @($package.PackageUserInformation)) {
      $sid = [string]$user.UserSecurityId
      if ($sid -match '\AS-\d+(?:-\d+)+\z') { [void]$sids.Add($sid) }
    }
    foreach ($sid in $sids) {
      [void]$keys.Add(('HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Appx\AppxAllUserStore\EndOfLife\{0}\{1}' -f $sid, $fullName))
    }
  }
  $regPath = [System.IO.Path]::Combine($logDirectory, ('CuratedAppx-retirement-{0}.reg' -f ([Guid]::NewGuid().ToString('N'))))
  $lines = [System.Collections.Generic.List[string]]::new()
  $lines.Add('Windows Registry Editor Version 5.00')
  $lines.Add('')
  foreach ($key in @($keys)) { $lines.Add(('[' + $key + ']')); $lines.Add('') }
  try {
    [System.IO.File]::WriteAllLines($regPath, $lines, [System.Text.UnicodeEncoding]::new($false, $true))
    $regInfo = Get-Item -LiteralPath $regPath -Force -ErrorAction Stop
    if (-not ($regInfo -is [System.IO.FileInfo]) -or
        (($regInfo.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) -or
        $regInfo.Length -le 40 -or $regInfo.Length -gt 524288) {
      throw 'retirement marker file failed regular-file validation'
    }
    $regExe = [System.IO.Path]::Combine($env:SystemRoot, 'System32', 'reg.exe')
    & $regExe import $regPath | Out-Null
    if ($LASTEXITCODE -ne 0) { throw ('reg.exe import failed with exit code {0}' -f $LASTEXITCODE) }
  } finally {
    if ([System.IO.File]::Exists($regPath)) { [System.IO.File]::Delete($regPath) }
  }
}

Write-Host '[LetRecovery] Preinstalled app cleanup: removing exact provisioned identities...'
try {
  $provisioned = @(Get-AppxProvisionedPackage -Online -ErrorAction Stop)
  $registered = @(Get-AppxPackage -AllUsers -ErrorAction Stop)
  foreach ($identity in $allowed) {
    $retirementPackages = [System.Collections.Generic.List[object]]::new()
    foreach ($package in $provisioned) {
      if ([string]::Equals([string]$package.DisplayName, [string]$identity.Name, [System.StringComparison]::Ordinal)) { $retirementPackages.Add($package) }
    }
    foreach ($package in $registered) {
      if ([string]::Equals([string]$package.PackageFamilyName, [string]$identity.Family, [System.StringComparison]::OrdinalIgnoreCase)) { $retirementPackages.Add($package) }
    }
    if ($retirementPackages.Count -gt 0) {
      try { Import-LetRecoveryAppxRetirementMarkers $identity.Family @($retirementPackages) }
      catch { $diagnostics.Add(('markers:{0}:0x{1:X8}' -f $identity.Family, [uint32]$_.Exception.HResult)) }
      try { Set-NonRemovableAppsPolicy -Online -PackageFamilyName $identity.Family -NonRemovable 0 -ErrorAction Stop | Out-Null }
      catch { $diagnostics.Add(('policy:{0}:0x{1:X8}' -f $identity.Family, [uint32]$_.Exception.HResult)) }
    }
    foreach ($package in $provisioned) {
      if ([string]::Equals([string]$package.DisplayName, [string]$identity.Name, [System.StringComparison]::Ordinal)) {
        try { Remove-AppxProvisionedPackage -Online -PackageName ([string]$package.PackageName) -ErrorAction Stop | Out-Null }
        catch { $diagnostics.Add(('deprovision:{0}:0x{1:X8}' -f $identity.Family, [uint32]$_.Exception.HResult)) }
      }
    }
  }
  Write-Host '[LetRecovery] Preinstalled app cleanup: removing all-user registrations...'
  foreach ($identity in $allowed) {
    foreach ($package in $registered) {
      if ([string]::Equals([string]$package.PackageFamilyName, [string]$identity.Family, [System.StringComparison]::OrdinalIgnoreCase)) {
        try { Remove-AppxPackage -Package ([string]$package.PackageFullName) -AllUsers -Confirm:$false -ErrorAction Stop }
        catch { $diagnostics.Add(('remove:{0}:0x{1:X8}' -f $identity.Family, [uint32]$_.Exception.HResult)) }
      }
    }
  }
  Write-Host '[LetRecovery] Preinstalled app cleanup: verifying final provisioning and all-user state...'
  $finalProvisioned = @(Get-AppxProvisionedPackage -Online -ErrorAction Stop)
  $finalRegistered = @(Get-AppxPackage -AllUsers -ErrorAction Stop)
  foreach ($identity in $allowed) {
    foreach ($package in $finalProvisioned) {
      if ([string]::Equals([string]$package.DisplayName, [string]$identity.Name, [System.StringComparison]::Ordinal)) {
        $failures.Add(('provisioned_still_present:{0}' -f $identity.Family))
      }
    }
    foreach ($package in $finalRegistered) {
      if ([string]::Equals([string]$package.PackageFamilyName, [string]$identity.Family, [System.StringComparison]::OrdinalIgnoreCase)) {
        $failures.Add(('registered_still_present:{0}' -f $identity.Family))
      }
    }
  }
} catch {
  $failures.Add(('inventory_failed:0x{0:X8}' -f [uint32]$_.Exception.HResult))
}
$failures = @($failures | Sort-Object -Unique)
$diagnostics = @($diagnostics | Sort-Object -Unique)
foreach ($diagnostic in $diagnostics) { Write-Host ('[LetRecovery] Preinstalled app cleanup diagnostic: {0}' -f $diagnostic) }
if ($failures.Count -ne 0) {
  foreach ($failure in $failures) { [Console]::Error.WriteLine(('LETRECOVERY_APPX_WARNING {0}' -f $failure)) }
} else {
  Write-Host '[LetRecovery] Preinstalled app cleanup: completed and verified.'
}
exit 0
"#;

pub fn curated_online_script_path(target_partition: &str) -> AnyResult<PathBuf> {
    let root = normalized_script_target_root(target_partition)?;
    Ok(root
        .join("LetRecovery_Scripts")
        .join(CURATED_ONLINE_SCRIPT_FILE_NAME))
}

pub fn stage_curated_online_removal_script(target_partition: &str) -> AnyResult<PathBuf> {
    let target = curated_online_script_path(target_partition)?;
    let directory = target
        .parent()
        .context("curated AppX script has no parent")?;
    std::fs::create_dir_all(directory)?;
    reject_script_reparse_or_non_directory(directory)?;
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-curated-appx",
        "ps1",
        CURATED_ONLINE_REMOVAL_SCRIPT.as_bytes(),
    )?;
    temporary.persist_replace(&target)?;
    if !curated_online_script_is_staged(target_partition)? {
        anyhow::bail!("published curated AppX online script readback mismatch");
    }
    Ok(target)
}

pub fn curated_online_script_is_staged(target_partition: &str) -> AnyResult<bool> {
    let path = curated_online_script_path(target_partition)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || script_metadata_is_reparse_point(&metadata) {
        anyhow::bail!("curated AppX online script is not a regular file");
    }
    Ok(std::fs::read(path)? == CURATED_ONLINE_REMOVAL_SCRIPT.as_bytes())
}

pub fn render_curated_specialize_command(order: u32) -> AnyResult<String> {
    let path = format!(
        r#"powershell.exe -NoP -NonI -W Hidden -EP Bypass -File "%SystemDrive%\LetRecovery_Scripts\{CURATED_ONLINE_SCRIPT_FILE_NAME}""#
    );
    crate::unattend_command::render_specialize_run_synchronous_command(
        order,
        &path,
        "Remove preinstalled applications",
    )
}

fn normalized_script_target_root(target_partition: &str) -> AnyResult<PathBuf> {
    let value = target_partition.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        anyhow::bail!("target partition must be a drive letter");
    }
    let root = PathBuf::from(format!("{}\\", value.to_ascii_uppercase()));
    if !root.join("Windows\\System32\\config\\SOFTWARE").is_file() {
        anyhow::bail!("target does not contain a complete offline SOFTWARE hive");
    }
    Ok(root)
}

fn reject_script_reparse_or_non_directory(path: &Path) -> AnyResult<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || script_metadata_is_reparse_point(&metadata) {
        anyhow::bail!("curated AppX script directory is not a regular directory");
    }
    Ok(())
}

fn script_metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0000_0400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineProvisionedPackage {
    pub package_name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPackageIdentity {
    pub package_name: String,
    pub package_family_name: String,
}

pub trait PackageIdentityResolver {
    fn resolve(&self, package_full_name: &str) -> Result<ResolvedPackageIdentity, String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CuratedAppxStatus {
    Removed,
    NotPresent,
    Warning,
}

impl CuratedAppxStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::NotPresent => "not_present",
            Self::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuratedAppxItemResult {
    pub id: String,
    pub package_full_name: Option<String>,
    pub status: CuratedAppxStatus,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CuratedAppxRemovalReport {
    pub removed: usize,
    pub not_present: usize,
    pub warnings: usize,
    pub items: Vec<CuratedAppxItemResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfflineAppxError {
    InvalidOfflineTarget(String),
    Inventory(String),
}

impl std::fmt::Display for OfflineAppxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOfflineTarget(target) => {
                write!(formatter, "invalid offline Windows target: {target:?}")
            }
            Self::Inventory(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for OfflineAppxError {}

pub fn remove_curated_preinstalled_appx(
    target: &str,
) -> Result<CuratedAppxRemovalReport, OfflineAppxError> {
    remove_exact_provisioned_appx(target, CURATED_PREINSTALLED_APPX)
}

pub fn remove_curated_preinstalled_appx_with(
    executor: &dyn CommandExecutor,
    identity_resolver: &dyn PackageIdentityResolver,
    target: &str,
) -> Result<CuratedAppxRemovalReport, OfflineAppxError> {
    remove_exact_provisioned_appx_with(
        executor,
        identity_resolver,
        target,
        CURATED_PREINSTALLED_APPX,
    )
}

/// Remove provisioned packages whose runtime-derived identity exactly matches `allowed`.
///
/// This is intentionally an identity allowlist API, not a general package-name removal API. It is
/// shared with other narrowly scoped offline servicing features that need the same fresh
/// inventory, identity verification, exact DISM argument, and post-removal readback guarantees.
pub fn remove_exact_provisioned_appx(
    target: &str,
    allowed: &[CuratedAppxIdentity],
) -> Result<CuratedAppxRemovalReport, OfflineAppxError> {
    remove_exact_provisioned_appx_with(
        &SystemCommandExecutor,
        &WindowsPackageIdentityResolver,
        target,
        allowed,
    )
}

pub fn remove_exact_provisioned_appx_with(
    executor: &dyn CommandExecutor,
    identity_resolver: &dyn PackageIdentityResolver,
    target: &str,
    allowed: &[CuratedAppxIdentity],
) -> Result<CuratedAppxRemovalReport, OfflineAppxError> {
    let image = normalize_offline_root(target)?;
    // One fresh baseline avoids launching DISM once per absent allowlist item. Every actual
    // mutation still gets its own immediately-before inventory and mandatory after readback.
    let baseline = inventory(executor, &image)?;

    let mut report = CuratedAppxRemovalReport::default();
    for allowed in allowed {
        let prefix = format!("{}_", allowed.package_name);
        let possible = baseline
            .iter()
            .filter(|package| starts_with_ignore_ascii_case(&package.package_name, &prefix))
            .cloned()
            .collect::<Vec<_>>();

        if possible.is_empty() {
            push_result(
                &mut report,
                CuratedAppxItemResult {
                    id: allowed.id.to_owned(),
                    package_full_name: None,
                    status: CuratedAppxStatus::NotPresent,
                    reason: "fresh_inventory_has_no_exact_identity_name_candidate".to_owned(),
                },
            );
            continue;
        }

        let mut authorized = Vec::new();
        for package in possible {
            match identity_resolver.resolve(&package.package_name) {
                Ok(identity)
                    if identity.package_name == allowed.package_name
                        && identity
                            .package_family_name
                            .eq_ignore_ascii_case(allowed.package_family_name) =>
                {
                    authorized.push(package.package_name);
                }
                Ok(identity) => push_result(
                    &mut report,
                    CuratedAppxItemResult {
                        id: allowed.id.to_owned(),
                        package_full_name: Some(package.package_name),
                        status: CuratedAppxStatus::Warning,
                        reason: format!(
                            "package_identity_mismatch:name={:?},family={:?}",
                            identity.package_name, identity.package_family_name
                        ),
                    },
                ),
                Err(error) => push_result(
                    &mut report,
                    CuratedAppxItemResult {
                        id: allowed.id.to_owned(),
                        package_full_name: Some(package.package_name),
                        status: CuratedAppxStatus::Warning,
                        reason: format!("package_identity_api_failed:{error}"),
                    },
                ),
            }
        }

        if authorized.is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        for package_full_name in authorized {
            if !seen.insert(package_full_name.to_ascii_lowercase()) {
                continue;
            }
            remove_one_and_read_back(executor, &image, allowed, &package_full_name, &mut report);
        }
    }
    Ok(report)
}

fn remove_one_and_read_back(
    executor: &dyn CommandExecutor,
    image: &str,
    allowed: &CuratedAppxIdentity,
    package_full_name: &str,
    report: &mut CuratedAppxRemovalReport,
) {
    if !valid_package_full_name(package_full_name) {
        push_result(
            report,
            warning(
                allowed,
                package_full_name,
                "invalid_inventory_package_full_name",
            ),
        );
        return;
    }

    // Refresh immediately before mutation. Never act on a stale package name.
    match inventory(executor, image) {
        Ok(fresh)
            if !fresh
                .iter()
                .any(|package| package.package_name.eq_ignore_ascii_case(package_full_name)) =>
        {
            push_result(
                report,
                CuratedAppxItemResult {
                    id: allowed.id.to_owned(),
                    package_full_name: Some(package_full_name.to_owned()),
                    status: CuratedAppxStatus::NotPresent,
                    reason: "package_disappeared_before_mutation".to_owned(),
                },
            );
            return;
        }
        Err(error) => {
            push_result(
                report,
                warning(
                    allowed,
                    package_full_name,
                    &format!("pre_remove_inventory_failed:{error}"),
                ),
            );
            return;
        }
        Ok(_) => {}
    }

    let request = CommandRequest::new("dism.exe").args([
        "/English".to_owned(),
        format!("/Image:{image}"),
        "/Remove-ProvisionedAppxPackage".to_owned(),
        format!("/PackageName:{package_full_name}"),
    ]);
    let execution = executor.execute(&request);

    // A fresh readback is mandatory even after a process-start or DISM failure.
    let readback = inventory(executor, image);
    let still_present = match &readback {
        Ok(fresh) => fresh
            .iter()
            .any(|package| package.package_name.eq_ignore_ascii_case(package_full_name)),
        Err(error) => {
            push_result(
                report,
                warning(
                    allowed,
                    package_full_name,
                    &format!("post_remove_inventory_failed:{error}"),
                ),
            );
            return;
        }
    };

    match execution {
        Ok(outcome) => {
            let output = command_text(&outcome);
            if dism_succeeded(&outcome, &output) && !still_present {
                push_result(
                    report,
                    CuratedAppxItemResult {
                        id: allowed.id.to_owned(),
                        package_full_name: Some(package_full_name.to_owned()),
                        status: CuratedAppxStatus::Removed,
                        reason: "dism_success_and_fresh_readback_absent".to_owned(),
                    },
                );
            } else {
                let reason = if still_present {
                    format!(
                        "fresh_readback_still_present;{}",
                        dism_detail(&outcome, &output)
                    )
                } else {
                    format!(
                        "package_absent_but_dism_failed;{}",
                        dism_detail(&outcome, &output)
                    )
                };
                push_result(report, warning(allowed, package_full_name, &reason));
            }
        }
        Err(error) => push_result(
            report,
            warning(
                allowed,
                package_full_name,
                &format!("dism_process_start_failed:{error};readback_present={still_present}"),
            ),
        ),
    }
}

fn warning(
    allowed: &CuratedAppxIdentity,
    package_full_name: &str,
    reason: &str,
) -> CuratedAppxItemResult {
    CuratedAppxItemResult {
        id: allowed.id.to_owned(),
        package_full_name: Some(package_full_name.to_owned()),
        status: CuratedAppxStatus::Warning,
        reason: reason.to_owned(),
    }
}

fn push_result(report: &mut CuratedAppxRemovalReport, item: CuratedAppxItemResult) {
    match item.status {
        CuratedAppxStatus::Removed => report.removed += 1,
        CuratedAppxStatus::NotPresent => report.not_present += 1,
        CuratedAppxStatus::Warning => report.warnings += 1,
    }
    report.items.push(item);
}

fn inventory(
    executor: &dyn CommandExecutor,
    image: &str,
) -> Result<Vec<OfflineProvisionedPackage>, OfflineAppxError> {
    let request = CommandRequest::new("dism.exe").args([
        "/English".to_owned(),
        format!("/Image:{image}"),
        "/Get-ProvisionedAppxPackages".to_owned(),
    ]);
    let outcome = executor.execute(&request).map_err(|error| {
        OfflineAppxError::Inventory(format!("DISM inventory start failed: {error}"))
    })?;
    let output = command_text(&outcome);
    if !dism_succeeded(&outcome, &output) {
        return Err(OfflineAppxError::Inventory(dism_detail(&outcome, &output)));
    }
    parse_provisioned_packages(&output).map_err(|detail| {
        OfflineAppxError::Inventory(format!(
            "DISM inventory output is not trustworthy: {detail}"
        ))
    })
}

fn normalize_offline_root(target: &str) -> Result<String, OfflineAppxError> {
    let target = target.trim();
    match target.as_bytes() {
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:\\", (*letter as char).to_ascii_uppercase()))
        }
        [letter, b':', b'\\'] if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:\\", (*letter as char).to_ascii_uppercase()))
        }
        _ => Err(OfflineAppxError::InvalidOfflineTarget(target.to_owned())),
    }
}

fn valid_package_full_name(package: &str) -> bool {
    !package.is_empty()
        && package.len() <= 512
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~'))
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn command_text(outcome: &CommandOutcome) -> String {
    let mut output = crate::encoding::gbk_to_utf8(outcome.stdout());
    let stderr = crate::encoding::gbk_to_utf8(outcome.stderr());
    if !stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&stderr);
    }
    output
}

fn dism_succeeded(outcome: &CommandOutcome, output: &str) -> bool {
    outcome.succeeded()
        && !output.lines().any(|line| {
            let line = line.trim();
            line.starts_with("Error:")
                || line.starts_with("错误:")
                || line.eq_ignore_ascii_case("The operation failed.")
        })
}

fn dism_detail(outcome: &CommandOutcome, output: &str) -> String {
    let detail = output.trim();
    if detail.is_empty() {
        format!("dism_exit={:?}", outcome.exit_code())
    } else {
        let compact = detail.lines().collect::<Vec<_>>().join(" | ");
        format!("dism_exit={:?},output={compact:?}", outcome.exit_code())
    }
}

fn parse_provisioned_packages(output: &str) -> Result<Vec<OfflineProvisionedPackage>, String> {
    const INVENTORY_START: &str =
        "Getting the list of app packages (.appx or .appxbundle) in this image...";
    const SUCCESS: &str = "The operation completed successfully.";

    let lines = output.lines().map(str::trim).collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.eq_ignore_ascii_case(INVENTORY_START))
        .ok_or_else(|| "missing_inventory_start_marker".to_owned())?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| line.eq_ignore_ascii_case(SUCCESS).then_some(index))
        .ok_or_else(|| "missing_inventory_success_marker".to_owned())?;

    let mut packages = Vec::new();
    let mut package_name: Option<String> = None;
    let mut display_name: Option<String> = None;
    let mut saw_record_field = false;
    let flush = |packages: &mut Vec<OfflineProvisionedPackage>,
                 package_name: &mut Option<String>,
                 display_name: &mut Option<String>,
                 saw_record_field: &mut bool|
     -> Result<(), String> {
        if !*saw_record_field {
            return Ok(());
        }
        let package_name = package_name
            .take()
            .ok_or_else(|| "inventory_record_missing_package_name".to_owned())?;
        if !valid_package_full_name(&package_name) {
            return Err(format!("invalid_inventory_package_name:{package_name:?}"));
        }
        if packages
            .iter()
            .any(|existing| existing.package_name.eq_ignore_ascii_case(&package_name))
        {
            return Err(format!("duplicate_inventory_package_name:{package_name:?}"));
        }
        packages.push(OfflineProvisionedPackage {
            display_name: display_name.take().unwrap_or_default(),
            package_name,
        });
        *saw_record_field = false;
        Ok(())
    };
    for line in &lines[start + 1..end] {
        if line.is_empty() {
            flush(
                &mut packages,
                &mut package_name,
                &mut display_name,
                &mut saw_record_field,
            )?;
        } else if let Some(value) = line.strip_prefix("PackageName :") {
            if package_name.is_some() {
                return Err("inventory_record_has_multiple_package_names".to_owned());
            }
            saw_record_field = true;
            package_name = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("DisplayName :") {
            saw_record_field = true;
            display_name = Some(value.trim().to_owned());
        } else if line.starts_with("PackageName") || line.starts_with("DisplayName") {
            return Err(format!("unrecognized_inventory_field:{line:?}"));
        } else if ["Version :", "Architecture :", "ResourceId :", "Regions :"]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            saw_record_field = true;
        }
    }
    flush(
        &mut packages,
        &mut package_name,
        &mut display_name,
        &mut saw_record_field,
    )?;
    Ok(packages)
}

pub struct WindowsPackageIdentityResolver;

/// Windows package full names have five underscore-delimited identity components:
/// name, version, architecture, resource id, and publisher id. Package identity names cannot
/// contain underscores, so parsing from the right is unambiguous. This compatibility path exists
/// only because `PackageFamilyNameFromFullName` and `PackageIdFromFullName` require Windows 8;
/// it deliberately accepts a much narrower grammar than the general DISM output parser.
/// See <https://learn.microsoft.com/windows/apps/desktop/modernize/package-identity-overview>.
fn resolve_portable_package_identity(
    package_full_name: &str,
) -> Result<ResolvedPackageIdentity, String> {
    if !valid_package_full_name(package_full_name) {
        return Err("portable_identity_invalid_package_full_name".to_owned());
    }
    let mut components = package_full_name.rsplitn(5, '_');
    let publisher_id = components
        .next()
        .ok_or_else(|| "portable_identity_missing_publisher_id".to_owned())?;
    let resource_id = components
        .next()
        .ok_or_else(|| "portable_identity_missing_resource_id".to_owned())?;
    let architecture = components
        .next()
        .ok_or_else(|| "portable_identity_missing_architecture".to_owned())?;
    let version = components
        .next()
        .ok_or_else(|| "portable_identity_missing_version".to_owned())?;
    let package_name = components
        .next()
        .ok_or_else(|| "portable_identity_missing_name".to_owned())?;

    if !(3..=50).contains(&package_name.len())
        || !package_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err("portable_identity_invalid_name".to_owned());
    }
    let version_parts = version.split('.').collect::<Vec<_>>();
    if version_parts.len() != 4
        || version_parts
            .iter()
            .any(|part| part.is_empty() || part.parse::<u16>().is_err())
    {
        return Err("portable_identity_invalid_version".to_owned());
    }
    if !matches!(
        architecture,
        "neutral" | "x86" | "x64" | "arm" | "arm64" | "x86a64"
    ) {
        return Err("portable_identity_invalid_architecture".to_owned());
    }
    if resource_id.len() > 30
        || !(resource_id.is_empty()
            || resource_id == "~"
            || resource_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
    {
        return Err("portable_identity_invalid_resource_id".to_owned());
    }
    if publisher_id.len() != 13
        || !publisher_id.bytes().all(|byte| {
            matches!(
                byte,
                b'a'..=b'h'
                    | b'j'
                    | b'k'
                    | b'm'
                    | b'n'
                    | b'p'..=b't'
                    | b'v'..=b'z'
                    | b'0'..=b'9'
            )
        })
    {
        return Err("portable_identity_invalid_publisher_id".to_owned());
    }

    Ok(ResolvedPackageIdentity {
        package_name: package_name.to_owned(),
        package_family_name: format!("{package_name}_{publisher_id}"),
    })
}

#[cfg(windows)]
impl PackageIdentityResolver for WindowsPackageIdentityResolver {
    fn resolve(&self, package_full_name: &str) -> Result<ResolvedPackageIdentity, String> {
        resolve_windows_package_identity(package_full_name)
    }
}

#[cfg(not(windows))]
impl PackageIdentityResolver for WindowsPackageIdentityResolver {
    fn resolve(&self, _package_full_name: &str) -> Result<ResolvedPackageIdentity, String> {
        Err("package identity APIs are only available on Windows".to_owned())
    }
}

#[cfg(windows)]
fn resolve_windows_package_identity(
    package_full_name: &str,
) -> Result<ResolvedPackageIdentity, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    type FamilyNameFn = unsafe extern "system" fn(*const u16, *mut u32, *mut u16) -> i32;
    type PackageIdFn = unsafe extern "system" fn(*const u16, u32, *mut u32, *mut u8) -> i32;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    const MAX_IDENTITY_BUFFER: u32 = 64 * 1024;

    #[repr(C)]
    struct PackageId {
        reserved: u32,
        processor_architecture: u32,
        version: u64,
        name: *const u16,
        publisher: *const u16,
        resource_id: *const u16,
        publisher_id: *const u16,
    }

    let wide = OsStr::new(package_full_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let library = unsafe { libloading::Library::new("kernel32.dll") }
        .map_err(|error| format!("load kernel32.dll failed: {error}"))?;
    let family_fn = match unsafe { library.get::<FamilyNameFn>(b"PackageFamilyNameFromFullName\0") }
    {
        Ok(function) => function,
        Err(_) => return resolve_portable_package_identity(package_full_name),
    };
    let package_id_fn = match unsafe { library.get::<PackageIdFn>(b"PackageIdFromFullName\0") } {
        Ok(function) => function,
        Err(_) => return resolve_portable_package_identity(package_full_name),
    };

    let mut family_len = 0u32;
    let first = unsafe { family_fn(wide.as_ptr(), &mut family_len, std::ptr::null_mut()) };
    if first != ERROR_INSUFFICIENT_BUFFER || family_len == 0 || family_len > MAX_IDENTITY_BUFFER {
        return Err(format!(
            "PackageFamilyNameFromFullName size query failed: code={first}, length={family_len}"
        ));
    }
    let mut family = vec![0u16; family_len as usize];
    let second = unsafe { family_fn(wide.as_ptr(), &mut family_len, family.as_mut_ptr()) };
    if second != ERROR_SUCCESS {
        return Err(format!(
            "PackageFamilyNameFromFullName failed: code={second}"
        ));
    }
    let family_end = family
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(family.len());
    let package_family_name = String::from_utf16(&family[..family_end])
        .map_err(|error| format!("invalid package family UTF-16: {error}"))?;

    let mut id_bytes = 0u32;
    let first = unsafe { package_id_fn(wide.as_ptr(), 0, &mut id_bytes, std::ptr::null_mut()) };
    if first != ERROR_INSUFFICIENT_BUFFER
        || id_bytes < std::mem::size_of::<PackageId>() as u32
        || id_bytes > MAX_IDENTITY_BUFFER
    {
        return Err(format!(
            "PackageIdFromFullName size query failed: code={first}, length={id_bytes}"
        ));
    }
    let word_count = (id_bytes as usize).div_ceil(std::mem::size_of::<usize>());
    let mut aligned = vec![0usize; word_count];
    let buffer = aligned.as_mut_ptr().cast::<u8>();
    let second = unsafe { package_id_fn(wide.as_ptr(), 0, &mut id_bytes, buffer) };
    if second != ERROR_SUCCESS {
        return Err(format!("PackageIdFromFullName failed: code={second}"));
    }
    let package_id = unsafe { &*buffer.cast::<PackageId>() };
    let package_name =
        unsafe { wide_string_inside_buffer(package_id.name, buffer, id_bytes as usize) }?;
    if package_name.is_empty() || package_family_name.is_empty() {
        return Err("package identity API returned an empty identity".to_owned());
    }
    Ok(ResolvedPackageIdentity {
        package_name,
        package_family_name,
    })
}

#[cfg(windows)]
unsafe fn wide_string_inside_buffer(
    pointer: *const u16,
    buffer: *const u8,
    byte_len: usize,
) -> Result<String, String> {
    if pointer.is_null() || byte_len < 2 {
        return Err("PackageId contains a null or empty name pointer".to_owned());
    }
    let start = buffer as usize;
    let end = start
        .checked_add(byte_len)
        .ok_or_else(|| "PackageId buffer range overflow".to_owned())?;
    let address = pointer as usize;
    if address < start || address >= end || !address.is_multiple_of(std::mem::align_of::<u16>()) {
        return Err("PackageId name pointer is outside the returned buffer".to_owned());
    }
    let units = (end - address) / 2;
    let slice = unsafe { std::slice::from_raw_parts(pointer, units) };
    let nul = slice
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| "PackageId name is not NUL-terminated inside the buffer".to_owned())?;
    String::from_utf16(&slice[..nul]).map_err(|error| format!("invalid PackageId UTF-16: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::sync::Mutex;

    struct SequenceExecutor {
        outcomes: Mutex<VecDeque<CommandOutcome>>,
        requests: Mutex<Vec<CommandRequest>>,
    }

    impl SequenceExecutor {
        fn new(outcomes: Vec<CommandOutcome>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandExecutor for SequenceExecutor {
        fn execute(&self, request: &CommandRequest) -> io::Result<CommandOutcome> {
            self.requests.lock().unwrap().push(request.clone());
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no mock outcome"))
        }
    }

    struct StickyNotesIdentity;
    impl PackageIdentityResolver for StickyNotesIdentity {
        fn resolve(&self, _package_full_name: &str) -> Result<ResolvedPackageIdentity, String> {
            Ok(ResolvedPackageIdentity {
                package_name: "Microsoft.MicrosoftStickyNotes".to_owned(),
                package_family_name: "Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe".to_owned(),
            })
        }
    }

    fn outcome(inventory: Option<&str>) -> CommandOutcome {
        CommandOutcome::new(
            Some(0),
            inventory.unwrap_or_default().as_bytes().to_vec(),
            Vec::new(),
        )
    }

    const STICKY: &str = "Microsoft.MicrosoftStickyNotes_6.1.0.0_neutral_~_8wekyb3d8bbwe";
    fn sticky_inventory() -> String {
        format!(
            "Deployment Image Servicing and Management tool\r\n\
             Version: 10.0.26100.1\r\n\r\n\
             Image Version: 10.0.26100.1\r\n\r\n\
             Getting the list of app packages (.appx or .appxbundle) in this image...\r\n\r\n\
             DisplayName : Sticky Notes\r\n\
             Version : 6.1.0.0\r\n\
             Architecture : neutral\r\n\
             ResourceId : ~\r\n\
             PackageName : {STICKY}\r\n\
             Regions : all\r\n\r\n\
             The operation completed successfully.\r\n"
        )
    }

    fn empty_inventory() -> &'static str {
        "Deployment Image Servicing and Management tool\r\n\
         Version: 10.0.26100.1\r\n\r\n\
         Image Version: 10.0.26100.1\r\n\r\n\
         Getting the list of app packages (.appx or .appxbundle) in this image...\r\n\r\n\
         The operation completed successfully.\r\n"
    }

    #[test]
    fn exact_identity_is_removed_and_verified_by_fresh_readback() {
        let present = sticky_inventory();
        let outcomes = vec![
            outcome(Some(&present)),          // initial inventory
            outcome(Some(&present)),          // immediately before remove
            outcome(None),                    // DISM remove
            outcome(Some(empty_inventory())), // post-remove readback
        ];
        let executor = SequenceExecutor::new(outcomes);
        let report =
            remove_curated_preinstalled_appx_with(&executor, &StickyNotesIdentity, "d:").unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(report.warnings, 0);
        let requests = executor.requests.lock().unwrap();
        let removal = requests
            .iter()
            .find(|request| {
                request.arguments().iter().any(|argument| {
                    argument == &std::ffi::OsString::from("/Remove-ProvisionedAppxPackage")
                })
            })
            .unwrap();
        assert!(removal
            .arguments()
            .contains(&std::ffi::OsString::from(format!("/PackageName:{STICKY}"))));
    }

    #[test]
    fn api_identity_mismatch_never_reaches_remove() {
        struct WrongPublisher;
        impl PackageIdentityResolver for WrongPublisher {
            fn resolve(&self, _package_full_name: &str) -> Result<ResolvedPackageIdentity, String> {
                Ok(ResolvedPackageIdentity {
                    package_name: "Microsoft.MicrosoftStickyNotes".to_owned(),
                    package_family_name: "Microsoft.MicrosoftStickyNotes_untrusted".to_owned(),
                })
            }
        }
        let present = sticky_inventory();
        let executor = SequenceExecutor::new(vec![outcome(Some(&present))]);
        let report =
            remove_curated_preinstalled_appx_with(&executor, &WrongPublisher, "D:\\").unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.warnings, 1);
        assert!(executor
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request
                .arguments()
                .contains(&std::ffi::OsString::from("/Remove-ProvisionedAppxPackage"))));
    }

    #[test]
    fn successful_exit_is_a_warning_when_fresh_readback_still_contains_package() {
        let present = sticky_inventory();
        let executor = SequenceExecutor::new(vec![
            outcome(Some(&present)),
            outcome(Some(&present)),
            outcome(None),
            outcome(Some(&present)),
        ]);
        let report = remove_exact_provisioned_appx_with(
            &executor,
            &StickyNotesIdentity,
            "D:",
            &[CURATED_PREINSTALLED_APPX
                .iter()
                .copied()
                .find(|identity| identity.id == "sticky_notes")
                .unwrap()],
        )
        .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.warnings, 1);
        assert!(report.items[0]
            .reason
            .starts_with("fresh_readback_still_present"));
    }

    #[test]
    fn preinstalled_allowlist_is_exact_and_covers_the_supported_cleanup_scope() {
        assert_eq!(CURATED_PREINSTALLED_APPX.len(), 16);
        let mut families = std::collections::HashSet::new();
        assert!(CURATED_PREINSTALLED_APPX
            .iter()
            .all(|identity| !identity.package_family_name.contains('*')
                && families.insert(identity.package_family_name.to_ascii_lowercase())));
        for id in [
            "quick_assist",
            "sound_recorder",
            "microsoft_365",
            "clipchamp",
            "news",
            "phone_link",
            "solitaire",
            "clock",
            "weather",
            "microsoft_pc_manager",
            "get_started",
        ] {
            assert!(CURATED_PREINSTALLED_APPX.iter().any(|item| item.id == id));
        }
        assert!(!CURATED_PREINSTALLED_APPX.iter().any(|identity| {
            identity.package_name.contains("Store")
                || identity.package_name.contains("Framework")
                || identity.package_name.contains('*')
        }));
        assert!(!CURATED_PREINSTALLED_APPX.iter().any(|identity| {
            matches!(
                identity.package_name,
                "Microsoft.OneDriveSync" | "Microsoft.OutlookForWindows"
            )
        }));
    }

    #[test]
    fn online_cleanup_uses_exact_identities_and_final_fresh_state() {
        assert!(!CURATED_ONLINE_REMOVAL_SCRIPT.contains(" -like "));
        assert!(!CURATED_ONLINE_REMOVAL_SCRIPT.contains("Remove-Item"));
        assert!(!CURATED_ONLINE_REMOVAL_SCRIPT.contains("WindowsApps"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("Remove-AppxPackage -Package"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("Remove-AppxProvisionedPackage"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("registered_still_present"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("provisioned_still_present"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("AppxAllUserStore\\Deprovisioned"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("AppxAllUserStore\\EndOfLife"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.contains("LETRECOVERY_APPX_WARNING"));
        assert!(!CURATED_ONLINE_REMOVAL_SCRIPT.contains("exit 3"));
        assert!(CURATED_ONLINE_REMOVAL_SCRIPT.ends_with("exit 0\n"));
        assert!(!CURATED_ONLINE_REMOVAL_SCRIPT.contains("Microsoft.OneDriveSync"));
        assert!(!CURATED_ONLINE_REMOVAL_SCRIPT.contains("Microsoft.OutlookForWindows"));
        let command = render_curated_specialize_command(6).unwrap();
        assert!(!command.contains("<WillReboot>"));
        assert!(command.contains("-W Hidden"));
    }

    #[test]
    fn parser_rejects_command_metacharacters() {
        let parsed = parse_provisioned_packages(
            "Getting the list of app packages (.appx or .appxbundle) in this image...\r\n\r\n\
             PackageName : Microsoft.Good_1.0.0.0_x64__pub\r\n\r\n\
             PackageName : Bad & whoami\r\n\r\n\
             The operation completed successfully.\r\n",
        );
        assert_eq!(
            parsed.unwrap_err(),
            "invalid_inventory_package_name:\"Bad & whoami\""
        );
    }

    #[test]
    fn valid_empty_inventory_is_distinct_from_unparseable_output() {
        assert_eq!(
            parse_provisioned_packages(empty_inventory()).unwrap(),
            Vec::<OfflineProvisionedPackage>::new()
        );

        let truncated =
            "Getting the list of app packages (.appx or .appxbundle) in this image...\r\n\r\n\
                         PackageName : Microsoft.Good_1.0.0.0_x64__pub\r\n";
        assert_eq!(
            parse_provisioned_packages(truncated).unwrap_err(),
            "missing_inventory_success_marker"
        );

        let malformed =
            "Getting the list of app packages (.appx or .appxbundle) in this image...\r\n\r\n\
                         DisplayName : Microsoft.Good\r\n\r\n\
                         The operation completed successfully.\r\n";
        assert_eq!(
            parse_provisioned_packages(malformed).unwrap_err(),
            "inventory_record_missing_package_name"
        );
    }

    #[test]
    fn truncated_post_remove_inventory_never_reports_removed() {
        let present = sticky_inventory();
        let truncated =
            "Getting the list of app packages (.appx or .appxbundle) in this image...\r\n";
        let executor = SequenceExecutor::new(vec![
            outcome(Some(&present)),
            outcome(Some(&present)),
            outcome(None),
            outcome(Some(truncated)),
        ]);
        let report = remove_exact_provisioned_appx_with(
            &executor,
            &StickyNotesIdentity,
            "D:",
            &[CURATED_PREINSTALLED_APPX
                .iter()
                .copied()
                .find(|identity| identity.id == "sticky_notes")
                .unwrap()],
        )
        .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.warnings, 1);
        assert!(report.items[0]
            .reason
            .starts_with("post_remove_inventory_failed:"));
    }

    #[cfg(windows)]
    #[test]
    fn documented_identity_apis_parse_provisioned_bundle_names() {
        let identity = WindowsPackageIdentityResolver.resolve(STICKY).unwrap();
        assert_eq!(identity.package_name, "Microsoft.MicrosoftStickyNotes");
        assert_eq!(
            identity.package_family_name,
            "Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe"
        );
    }

    #[test]
    fn win7_compatible_parser_derives_the_same_exact_identity() {
        let identity = resolve_portable_package_identity(STICKY).unwrap();
        assert_eq!(identity.package_name, "Microsoft.MicrosoftStickyNotes");
        assert_eq!(
            identity.package_family_name,
            "Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe"
        );
        let empty_resource = resolve_portable_package_identity(
            "Microsoft.MicrosoftStickyNotes_6.1.0.0_x64__8wekyb3d8bbwe",
        )
        .unwrap();
        assert_eq!(
            identity.package_family_name,
            empty_resource.package_family_name
        );
        assert!(resolve_portable_package_identity(
            "Microsoft.MicrosoftStickyNotes_6.1.0.0_x86a64__8wekyb3d8bbwe"
        )
        .is_ok());
    }

    #[test]
    fn win7_compatible_parser_rejects_ambiguous_or_noncanonical_names() {
        for value in [
            "Microsoft_MicrosoftStickyNotes_6.1.0.0_x64__8wekyb3d8bbwe",
            "Microsoft.MicrosoftStickyNotes_6.1.0_x64__8wekyb3d8bbwe",
            "Microsoft.MicrosoftStickyNotes_6.1.0.0_unknown__8wekyb3d8bbwe",
            "Microsoft.MicrosoftStickyNotes_6.1.0.0_x64__8wekyb3d8bbw",
            "Microsoft.MicrosoftStickyNotes_6.1.0.0_x64__iwekyb3d8bbwe",
            "Microsoft.MicrosoftStickyNotes_6.1.0.0_x64_bad_resource_8wekyb3d8bbwe",
        ] {
            assert!(resolve_portable_package_identity(value).is_err(), "{value}");
        }
    }
}
