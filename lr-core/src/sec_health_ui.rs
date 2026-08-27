//! Narrow Windows Security UI removal support for the Defender advanced option.
//!
//! The offline phase delegates to the exact-identity provisioned AppX boundary. The online
//! phase is a fixed, staged PowerShell script used only by LetRecovery's built-in Win10/11
//! unattend. It never deletes package directories or changes WindowsApps/AppRepository ACLs. The
//! script uses exact AppX retirement identities and can hide only the currently discoverable
//! non-mandatory KB5007651 Windows Update offer; it never claims a permanent update blacklist.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::offline_appx::{
    remove_exact_provisioned_appx, CuratedAppxIdentity, CuratedAppxRemovalReport, OfflineAppxError,
};

pub const ONLINE_SCRIPT_FILE_NAME: &str = "remove-sec-health-ui.ps1";

pub const SEC_HEALTH_UI_IDENTITIES: &[CuratedAppxIdentity] = &[
    CuratedAppxIdentity {
        id: "windows_security_ui_modern",
        package_name: "Microsoft.SecHealthUI",
        package_family_name: "Microsoft.SecHealthUI_8wekyb3d8bbwe",
    },
    CuratedAppxIdentity {
        id: "windows_security_ui_legacy",
        package_name: "Microsoft.Windows.SecHealthUI",
        package_family_name: "Microsoft.Windows.SecHealthUI_cw5n1h2txyewy",
    },
];

/// Fixed online force-removal script. It uses exact typed AppX identities, imports only narrowly
/// scoped AppxAllUserStore retirement keys, calls Microsoft's servicing cmdlets, attempts to hide
/// the current exact KB5007651 offer through Windows Update Agent, and records fresh readback. A
/// remaining package or unavailable update metadata is reported in the structured log but must not
/// abort Windows specialize after the target image has already been applied; first-logon performs
/// one more bounded retry before deleting the staging directory.
const ONLINE_REMOVAL_SCRIPT: &str = r#"[CmdletBinding()]
param(
    [switch]$SuppressCurrentSecurityUpdate
)

$ErrorActionPreference = 'Stop'
$result = [ordered]@{
    schema = 'LetRecovery.SecHealthUIRemoval.v1'
    status = 'warning'
    update_suppression = 'not_checked'
    items = [System.Collections.Generic.List[object]]::new()
}
Write-Host '[LetRecovery] Windows Security UI cleanup: reading the installed package inventory...'
$logDirectory = [System.IO.Path]::Combine($env:ProgramData, 'LetRecovery', 'Logs')
$logPath = [System.IO.Path]::Combine($logDirectory, 'SecHealthUI-removal.json')
$temporaryLogPath = $logPath + '.tmp'
$allowed = @(
    [ordered]@{ Name = 'Microsoft.SecHealthUI'; Family = 'Microsoft.SecHealthUI_8wekyb3d8bbwe' },
    [ordered]@{ Name = 'Microsoft.Windows.SecHealthUI'; Family = 'Microsoft.Windows.SecHealthUI_cw5n1h2txyewy' }
)

function Test-LetRecoveryAppxToken([string]$value) {
    return (-not [string]::IsNullOrWhiteSpace($value)) -and
        $value.Length -le 512 -and
        $value -match '\A[A-Za-z0-9._~-]+\z'
}

function Set-LetRecoverySecHealthUiUpdateSuppression() {
    # KB5007651 is Microsoft's Windows Security app/platform update. Windows Update Agent has no
    # supported permanent PFN blacklist: IsHidden applies only to a currently discoverable,
    # non-mandatory update object. A future update with a new identity must not be reported as
    # suppressed merely because today's offer was hidden.
    $targetKb = '5007651'
    try {
        $session = New-Object -ComObject 'Microsoft.Update.Session' -ErrorAction Stop
        $session.ClientApplicationID = 'LetRecovery.SecHealthUIRemoval'
        $searcher = $session.CreateUpdateSearcher()
        $searcher.Online = $true
        $searcher.IncludePotentiallySupersededUpdates = $true
        $searchResult = $searcher.Search("IsInstalled=0 and Type='Software'")
        $resultCode = [int]$searchResult.ResultCode
        if ($resultCode -ne 2 -and $resultCode -ne 3) {
            $result.update_suppression = 'warning'
            $result.items.Add([ordered]@{
                family = $null
                package = $null
                status = 'suppression_warning'
                reason = 'windows_update_search_failed'
                result_code = $resultCode
            })
            return
        }
        if ($resultCode -eq 3) {
            $result.update_suppression = 'warning'
            $result.items.Add([ordered]@{
                family = $null
                package = $null
                status = 'suppression_warning'
                reason = 'windows_update_search_succeeded_with_errors'
                result_code = $resultCode
            })
        }

        $matchingCount = 0
        for ($index = 0; $index -lt $searchResult.Updates.Count; $index++) {
            $update = $searchResult.Updates.Item($index)
            $isTarget = $false
            foreach ($kb in @($update.KBArticleIDs)) {
                if ([string]::Equals([string]$kb, $targetKb, [System.StringComparison]::Ordinal)) {
                    $isTarget = $true
                    break
                }
            }
            if (-not $isTarget) { continue }
            $matchingCount++

            $updateId = [string]$update.Identity.UpdateID
            $revision = [int]$update.Identity.RevisionNumber
            if ($update.IsMandatory -eq $true) {
                $result.update_suppression = 'warning'
                $result.items.Add([ordered]@{
                    family = $null
                    package = $null
                    status = 'suppression_warning'
                    reason = 'mandatory_update_cannot_be_hidden'
                    kb = $targetKb
                    update_id = $updateId
                    revision = $revision
                })
                continue
            }

            try {
                if ($update.IsHidden -ne $true) { $update.IsHidden = $true }
                if ($update.IsHidden -eq $true) {
                    if ($result.update_suppression -ne 'warning') {
                        $result.update_suppression = 'current_offer_hidden'
                    }
                    $result.items.Add([ordered]@{
                        family = $null
                        package = $null
                        status = 'current_update_offer_hidden'
                        reason = 'exact_kb_match_and_is_hidden_readback_true'
                        kb = $targetKb
                        update_id = $updateId
                        revision = $revision
                    })
                } else {
                    $result.update_suppression = 'warning'
                    $result.items.Add([ordered]@{
                        family = $null
                        package = $null
                        status = 'suppression_warning'
                        reason = 'is_hidden_readback_false'
                        kb = $targetKb
                        update_id = $updateId
                        revision = $revision
                    })
                }
            } catch {
                $result.update_suppression = 'warning'
                $result.items.Add([ordered]@{
                    family = $null
                    package = $null
                    status = 'suppression_warning'
                    reason = 'set_is_hidden_failed'
                    kb = $targetKb
                    update_id = $updateId
                    revision = $revision
                    exception_type = $_.Exception.GetType().FullName
                    hresult = $_.Exception.HResult
                })
            }
        }

        if ($matchingCount -eq 0) {
            if ($result.update_suppression -ne 'warning') {
                $result.update_suppression = 'no_current_offer'
            }
            $result.items.Add([ordered]@{
                family = $null
                package = $null
                status = 'no_current_update_offer'
                reason = 'exact_kb_not_present_in_current_search_result'
                kb = $targetKb
            })
        }
    } catch {
        $result.update_suppression = 'warning'
        $result.items.Add([ordered]@{
            family = $null
            package = $null
            status = 'suppression_warning'
            reason = 'windows_update_agent_exception'
            exception_type = $_.Exception.GetType().FullName
            hresult = $_.Exception.HResult
        })
    }
}

function Import-LetRecoveryAppxRetirementMarkers([string]$family, [object[]]$packages) {
    if (-not (Test-LetRecoveryAppxToken $family)) {
        throw ('unsafe package family name: {0}' -f $family)
    }
    [void][System.IO.Directory]::CreateDirectory($logDirectory)
    $directoryInfo = Get-Item -LiteralPath $logDirectory -Force -ErrorAction Stop
    if (-not $directoryInfo.PSIsContainer -or
        (($directoryInfo.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw 'retirement marker directory is not a regular directory'
    }

    $keys = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    [void]$keys.Add(('HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Appx\AppxAllUserStore\Deprovisioned\{0}' -f $family))
    foreach ($package in $packages) {
        $fullName = [string]$package.PackageFullName
        if ([string]::IsNullOrWhiteSpace($fullName)) { $fullName = [string]$package.PackageName }
        if (-not (Test-LetRecoveryAppxToken $fullName)) {
            throw ('unsafe package full name: {0}' -f $fullName)
        }
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

    $regPath = [System.IO.Path]::Combine($logDirectory, ('SecHealthUI-retirement-{0}.reg' -f ([Guid]::NewGuid().ToString('N'))))
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add('Windows Registry Editor Version 5.00')
    $lines.Add('')
    foreach ($key in @($keys)) {
        $lines.Add(('[' + $key + ']'))
        $lines.Add('')
    }
    try {
        [System.IO.File]::WriteAllLines($regPath, $lines, [System.Text.UnicodeEncoding]::new($false, $true))
        $regInfo = Get-Item -LiteralPath $regPath -Force -ErrorAction Stop
        if (-not ($regInfo -is [System.IO.FileInfo]) -or
            (($regInfo.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) -or
            $regInfo.Length -le 40 -or $regInfo.Length -gt 131072) {
            throw 'retirement marker file failed regular-file validation'
        }
        $regExe = [System.IO.Path]::Combine($env:SystemRoot, 'System32', 'reg.exe')
        & $regExe import $regPath | Out-Null
        if ($LASTEXITCODE -ne 0) { throw ('reg.exe import failed with exit code {0}' -f $LASTEXITCODE) }
    } finally {
        if ([System.IO.File]::Exists($regPath)) { [System.IO.File]::Delete($regPath) }
    }
}

try {
    if ($SuppressCurrentSecurityUpdate) {
        Write-Host '[LetRecovery] Windows Security UI cleanup: suppressing the current KB5007651 offer...'
        Set-LetRecoverySecHealthUiUpdateSuppression
    } else {
        $result.update_suppression = 'deferred_to_first_logon'
    }
    $inventory = @(Get-AppxPackage -AllUsers -ErrorAction Stop)
    $provisionedInventory = @(Get-AppxProvisionedPackage -Online -ErrorAction Stop)
    foreach ($identity in $allowed) {
        $candidates = [System.Collections.Generic.List[object]]::new()
        $provisionedCandidates = [System.Collections.Generic.List[object]]::new()
        foreach ($package in $inventory) {
            if ([string]::Equals([string]$package.Name, [string]$identity.Name, [System.StringComparison]::Ordinal) -and
                [string]::Equals([string]$package.PackageFamilyName, [string]$identity.Family, [System.StringComparison]::OrdinalIgnoreCase)) {
                $candidates.Add($package)
            }
        }
        foreach ($package in $provisionedInventory) {
            if ([string]::Equals([string]$package.DisplayName, [string]$identity.Name, [System.StringComparison]::Ordinal)) {
                $provisionedCandidates.Add($package)
            }
        }

        $markerPackages = [System.Collections.Generic.List[object]]::new()
        foreach ($package in $candidates) { $markerPackages.Add($package) }
        foreach ($package in $provisionedCandidates) { $markerPackages.Add($package) }
        if ($markerPackages.Count -gt 0) {
            try {
                Import-LetRecoveryAppxRetirementMarkers $identity.Family @($markerPackages)
                $result.items.Add([ordered]@{ family = $identity.Family; package = $null; status = 'markers_imported'; reason = 'exact_deprovisioned_and_end_of_life_keys_imported' })
            } catch {
                $result.items.Add([ordered]@{
                    family = $identity.Family
                    package = $null
                    status = 'attempt_warning'
                    reason = 'retirement_marker_import_failed'
                    exception_type = $_.Exception.GetType().FullName
                    hresult = $_.Exception.HResult
                })
            }
        }

        $requiresPolicyChange = $false
        foreach ($package in $candidates) {
            if ($package.NonRemovable -eq $true) { $requiresPolicyChange = $true }
        }
        if ($requiresPolicyChange -or $provisionedCandidates.Count -gt 0) {
            try {
                Set-NonRemovableAppsPolicy -Online -PackageFamilyName $identity.Family -NonRemovable 0 -ErrorAction Stop | Out-Null
                $result.items.Add([ordered]@{ family = $identity.Family; package = $null; status = 'policy_changed'; reason = 'official_non_removable_policy_set_to_zero' })
            } catch {
                $result.items.Add([ordered]@{
                    family = $identity.Family
                    package = $null
                    status = 'attempt_warning'
                    reason = 'set_non_removable_policy_failed'
                    exception_type = $_.Exception.GetType().FullName
                    hresult = $_.Exception.HResult
                })
            }
        }

        foreach ($package in $provisionedCandidates) {
            $packageName = [string]$package.PackageName
            $expectedPrefix = [string]$identity.Name + '_'
            if ([string]::IsNullOrWhiteSpace($packageName) -or
                -not $packageName.StartsWith($expectedPrefix, [System.StringComparison]::Ordinal) -or
                $packageName.IndexOfAny([char[]]@('\', '/', ':', [char]0)) -ge 0) {
                $result.items.Add([ordered]@{ family = $identity.Family; package = $packageName; status = 'attempt_warning'; reason = 'typed_provisioning_package_name_rejected' })
                continue
            }
            try {
                Remove-AppxProvisionedPackage -Online -PackageName $packageName -AllUsers -ErrorAction Stop | Out-Null
                $result.items.Add([ordered]@{ family = $identity.Family; package = $packageName; status = 'deprovision_attempted'; reason = 'official_online_deprovision_cmdlet_completed' })
            } catch {
                $result.items.Add([ordered]@{
                    family = $identity.Family
                    package = $packageName
                    status = 'attempt_warning'
                    reason = 'online_deprovision_cmdlet_failed'
                    exception_type = $_.Exception.GetType().FullName
                    hresult = $_.Exception.HResult
                })
            }
        }

        if ($candidates.Count -eq 0 -and $provisionedCandidates.Count -eq 0) {
            $result.items.Add([ordered]@{ family = $identity.Family; package = $null; status = 'not_present'; reason = 'fresh_inventory_and_provisioning_absent' })
        }

        foreach ($package in $candidates) {
            $fullName = [string]$package.PackageFullName
            $expectedPrefix = [string]$identity.Name + '_'
            if ([string]::IsNullOrWhiteSpace($fullName) -or
                -not $fullName.StartsWith($expectedPrefix, [System.StringComparison]::Ordinal) -or
                $fullName.IndexOfAny([char[]]@('\', '/', ':', [char]0)) -ge 0) {
                $result.items.Add([ordered]@{ family = $identity.Family; package = $fullName; status = 'warning'; reason = 'typed_inventory_full_name_rejected' })
                continue
            }

            $removeSucceeded = $false
            $removeExceptionType = $null
            $removeHresult = $null
            try {
                Remove-AppxPackage -Package $fullName -AllUsers -Confirm:$false -ErrorAction Stop
                $removeSucceeded = $true
            } catch {
                $removeExceptionType = $_.Exception.GetType().FullName
                $removeHresult = $_.Exception.HResult
            }

            try {
                $readback = @(Get-AppxPackage -AllUsers -ErrorAction Stop)
                $stillPresent = $false
                foreach ($remaining in $readback) {
                    if ([string]::Equals([string]$remaining.PackageFullName, $fullName, [System.StringComparison]::OrdinalIgnoreCase)) {
                        $stillPresent = $true
                    }
                }
                if ($removeSucceeded -and -not $stillPresent) {
                    $result.items.Add([ordered]@{ family = $identity.Family; package = $fullName; status = 'removed'; reason = 'cmdlet_success_and_fresh_readback_absent' })
                } elseif ($stillPresent) {
                    $reason = if ($removeSucceeded) { 'fresh_readback_still_present' } else { 'remove_cmdlet_failed_and_fresh_readback_present' }
                    $result.items.Add([ordered]@{ family = $identity.Family; package = $fullName; status = 'attempt_warning'; reason = $reason; exception_type = $removeExceptionType; hresult = $removeHresult })
                } else {
                    $result.items.Add([ordered]@{ family = $identity.Family; package = $fullName; status = 'removed'; reason = 'fresh_readback_absent_despite_remove_error'; exception_type = $removeExceptionType; hresult = $removeHresult })
                }
            } catch {
                $result.items.Add([ordered]@{
                    family = $identity.Family
                    package = $fullName
                    status = 'attempt_warning'
                    reason = 'post_remove_inventory_failed'
                    exception_type = $_.Exception.GetType().FullName
                    hresult = $_.Exception.HResult
                    remove_succeeded = $removeSucceeded
                })
            }
        }
    }
    Write-Host '[LetRecovery] Windows Security UI cleanup: verifying the final all-user and provisioning state...'
    $finalInventory = @(Get-AppxPackage -AllUsers -ErrorAction Stop)
    $finalProvisioned = @(Get-AppxProvisionedPackage -Online -ErrorAction Stop)
    $remainingCount = 0
    foreach ($identity in $allowed) {
        foreach ($remaining in $finalInventory) {
            if ([string]::Equals([string]$remaining.PackageFamilyName, [string]$identity.Family, [System.StringComparison]::OrdinalIgnoreCase)) {
                $remainingCount++
                $result.items.Add([ordered]@{ family = $identity.Family; package = [string]$remaining.PackageFullName; status = 'warning'; reason = 'final_all_user_registration_still_present' })
            }
        }
        foreach ($remaining in $finalProvisioned) {
            $provisionedName = [string]$remaining.DisplayName
            if ([string]::Equals($provisionedName, [string]$identity.Name, [System.StringComparison]::Ordinal)) {
                $remainingCount++
                $result.items.Add([ordered]@{ family = $identity.Family; package = [string]$remaining.PackageName; status = 'warning'; reason = 'final_provisioning_still_present' })
            }
        }
    }
    if ($remainingCount -eq 0) { $result.status = 'completed' }
} catch {
    $result.status = 'warning'
    $result.items.Add([ordered]@{
        family = $null
        package = $null
        status = 'warning'
        reason = 'online_inventory_failed'
        exception_type = $_.Exception.GetType().FullName
        hresult = $_.Exception.HResult
    })
}

if ($result.status -eq 'completed') {
    Write-Host '[LetRecovery] Windows Security UI cleanup: completed.'
} else {
    [Console]::Error.WriteLine('[LetRecovery] Windows Security UI cleanup did not reach the required absent state.')
}

try {
    [void][System.IO.Directory]::CreateDirectory($logDirectory)
    $json = ConvertTo-Json -InputObject $result -Depth 6 -Compress
    [System.IO.File]::WriteAllText($temporaryLogPath, $json, [System.Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporaryLogPath -Destination $logPath -Force
} catch {
    try {
        [Console]::Error.WriteLine(
            ('LETRECOVERY_SEC_HEALTH_UI_WARNING schema=LetRecovery.SecHealthUIRemoval.v1 code=structured_log_persist_failed exception_type={0} hresult={1}' -f
                $_.Exception.GetType().FullName,
                $_.Exception.HResult)
        )
    } catch {
    }
}
exit 0
"#;

pub fn remove_offline_provisioning(
    target: &str,
) -> Result<CuratedAppxRemovalReport, OfflineAppxError> {
    remove_exact_provisioned_appx(target, SEC_HEALTH_UI_IDENTITIES)
}

pub fn online_script_path(target_partition: &str) -> Result<PathBuf> {
    let root = normalized_target_root(target_partition)?;
    Ok(root
        .join("LetRecovery_Scripts")
        .join(ONLINE_SCRIPT_FILE_NAME))
}

/// Atomically stage the fixed online-removal script and read it back byte-for-byte.
pub fn stage_online_removal_script(target_partition: &str) -> Result<PathBuf> {
    let target = online_script_path(target_partition)?;
    let directory = target
        .parent()
        .context("SecHealthUI online script has no parent directory")?;
    std::fs::create_dir_all(directory).with_context(|| {
        format!(
            "create SecHealthUI script directory {}",
            directory.display()
        )
    })?;
    reject_reparse_or_non_directory(directory)?;

    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-sechealthui",
        "ps1",
        ONLINE_REMOVAL_SCRIPT.as_bytes(),
    )
    .context("stage SecHealthUI online removal script")?;
    if std::fs::read(temporary.path()).context("read back staged SecHealthUI script")?
        != ONLINE_REMOVAL_SCRIPT.as_bytes()
    {
        anyhow::bail!("staged SecHealthUI online script readback mismatch");
    }
    temporary
        .persist_replace(&target)
        .with_context(|| format!("publish SecHealthUI online script {}", target.display()))?;
    if !online_script_is_staged(target_partition)? {
        anyhow::bail!("published SecHealthUI online script readback mismatch");
    }
    Ok(target)
}

pub fn online_script_is_staged(target_partition: &str) -> Result<bool> {
    let path = online_script_path(target_partition)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!(
            "SecHealthUI online script is not a regular file: {}",
            path.display()
        );
    }
    Ok(
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))?
            == ONLINE_REMOVAL_SCRIPT.as_bytes(),
    )
}

/// Fixed specialize command for Windows 10/11. The script preserves a truthful structured result,
/// but it is deliberately not load-bearing after image application: aborting specialize here
/// produces an unusable installation and prevents the bounded first-logon retry from running.
pub fn render_specialize_command(order: u32) -> Result<String> {
    let path = format!(
        r#"powershell.exe -NoP -NonI -W Hidden -EP Bypass -File "%SystemDrive%\LetRecovery_Scripts\{ONLINE_SCRIPT_FILE_NAME}""#
    );
    crate::unattend_command::render_specialize_run_synchronous_command(
        order,
        &path,
        "Remove Windows Security UI package",
    )
}

fn normalized_target_root(target_partition: &str) -> Result<PathBuf> {
    let value = target_partition.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        anyhow::bail!("target partition must be a drive letter, got {target_partition:?}");
    }
    let root = PathBuf::from(format!("{}\\", value.to_ascii_uppercase()));
    if !root.join("Windows\\System32\\config\\SOFTWARE").is_file() {
        anyhow::bail!("target does not contain a complete offline SOFTWARE hive");
    }
    Ok(root)
}

fn reject_reparse_or_non_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect SecHealthUI script directory {}", path.display()))?;
    if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!(
            "SecHealthUI script directory is not a regular directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_exact_and_version_independent() {
        assert_eq!(SEC_HEALTH_UI_IDENTITIES.len(), 2);
        assert!(SEC_HEALTH_UI_IDENTITIES.iter().all(|identity| {
            !identity.package_name.contains('*')
                && !identity.package_family_name.contains('*')
                && !identity
                    .package_family_name
                    .chars()
                    .any(char::is_whitespace)
        }));
    }

    #[test]
    fn online_script_uses_exact_retirement_markers_without_package_directory_deletion() {
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("Remove-Item"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("AppxAllUserStore\\Deprovisioned"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("AppxAllUserStore\\EndOfLife"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("SystemApps"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("Where-Object"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("PackageFamilyName -like"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("WindowsApps"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("takeown"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("reg.exe"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("ReparsePoint"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Set-NonRemovableAppsPolicy -Online -PackageFamilyName $identity.Family -NonRemovable 0"));
        assert!(ONLINE_REMOVAL_SCRIPT
            .contains("Remove-AppxProvisionedPackage -Online -PackageName $packageName -AllUsers"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Remove-AppxPackage -Package $fullName -AllUsers"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("fresh_readback_still_present"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("post_remove_inventory_failed"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Windows Security UI cleanup"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Microsoft.Update.Session"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("[switch]$SuppressCurrentSecurityUpdate"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("deferred_to_first_logon"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("KB5007651"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$targetKb = '5007651'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("IsInstalled=0 and Type='Software'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("IncludePotentiallySupersededUpdates = $true"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$update.IsMandatory -eq $true"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$update.IsHidden = $true"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("exact_kb_match_and_is_hidden_readback_true"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("mandatory_update_cannot_be_hidden"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("windows_update_search_succeeded_with_errors"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("no_current_update_offer"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("AppLocker"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("DoNotConnectToWindowsUpdateInternetLocations"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("official_non_removable_policy_set_to_zero"));
        assert!(ONLINE_REMOVAL_SCRIPT
            .contains("if ($remainingCount -eq 0) { $result.status = 'completed' }"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("final_all_user_registration_still_present"));
        assert!(ONLINE_REMOVAL_SCRIPT.ends_with("exit 0\n"));
    }

    #[test]
    fn specialize_hook_is_fixed_and_rejects_zero_order() {
        assert!(render_specialize_command(0).is_err());
        let command = render_specialize_command(3).unwrap();
        assert!(command.contains("<Order>3</Order>"));
        assert!(command.contains(ONLINE_SCRIPT_FILE_NAME));
        assert!(command.contains("powershell.exe -NoP -NonI -W Hidden -EP Bypass"));
        assert!(!command.contains("SuppressCurrentSecurityUpdate"));
        assert!(!command.contains("<WillReboot>"));
        assert!(!command.contains("EncodedCommand"));

        let document = format!(
            r#"<root xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">{command}</root>"#
        );
        let parsed = roxmltree::Document::parse(&document).unwrap();
        let path = parsed
            .descendants()
            .find(|node| node.tag_name().name() == "Path")
            .and_then(|node| node.text())
            .unwrap();
        assert!(path.contains(ONLINE_SCRIPT_FILE_NAME));
        assert!(
            path.encode_utf16().count() <= crate::unattend_command::RUN_SYNCHRONOUS_PATH_MAX_UTF16
        );
    }

    #[test]
    fn script_has_a_setup_stderr_fallback_when_json_cannot_be_persisted() {
        assert!(ONLINE_REMOVAL_SCRIPT.contains("code=structured_log_persist_failed"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("[Console]::Error.WriteLine"));
        assert!(ONLINE_REMOVAL_SCRIPT.ends_with("exit 0\n"));
    }
}
