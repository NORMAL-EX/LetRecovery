//! Warning-only removal of the Win32 OneDrive sync client during Windows specialize.
//!
//! Microsoft documents `OneDriveSetup.exe /uninstall` as the supported setup boundary. This
//! module stages one fixed script for LetRecovery's built-in Windows 10/11 unattend. The script
//! only considers setup executables at the two Windows-owned locations used by supported x86/x64
//! installations, requires a valid Microsoft Authenticode signature, executes the single fixed
//! `/uninstall` argument, and performs fresh typed readback of fixed uninstall keys and executable
//! inventory. It never searches PATH, deletes directories, or edits registry state.
//!
//! Microsoft reference checked for this boundary:
//! <https://learn.microsoft.com/en-us/troubleshoot/sharepoint/lists-and-libraries/cannot-open-onedrive-on-images-using-sysprep>
//! <https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/start-process?view=powershell-5.1>
//! <https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.process.waitforexit?view=netframework-4.8>

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const ONLINE_SCRIPT_FILE_NAME: &str = "remove-onedrive-win32.ps1";

/// Fixed best-effort online operation. Every feature-level failure is recorded as structured
/// warning data and the script exits zero so this optional cleanup cannot block Windows Setup.
const ONLINE_REMOVAL_SCRIPT: &str = r#"[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$operationTimeoutMilliseconds = 120000
$result = [ordered]@{
    schema = 'LetRecovery.OneDriveWin32Removal.v1'
    status = 'warning'
    reason = 'not_started'
    candidates = [System.Collections.Generic.List[object]]::new()
    attempts = [System.Collections.Generic.List[object]]::new()
    selected_path = $null
    selected_sha256 = $null
    uninstaller_exit_code = $null
    readback_scope = 'fresh_specialize_machine_and_current_system_profile_only'
    future_user_registration_proven_absent = $false
    before = $null
    after = $null
    exception_type = $null
    hresult = $null
}

Write-Host '[LetRecovery] OneDrive cleanup: inspecting the official Windows uninstaller...'

function Write-SetupWarning {
    param(
        [Parameter(Mandatory = $true)][string]$Code,
        [Parameter(Mandatory = $true)][string]$ExceptionType,
        [Parameter(Mandatory = $true)][int]$HResult
    )

    try {
        [Console]::Error.WriteLine(
            'LETRECOVERY_ONEDRIVE_WARNING schema=LetRecovery.OneDriveWin32Removal.v1 code={0} exception_type={1} hresult={2}' -f
                $Code,
                $ExceptionType.Replace(' ', '_'),
                $HResult
        )
    } catch {
    }
}

$logDirectory = $null
$logPath = $null
$temporaryLogPath = $null
try {
    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) {
        throw [System.IO.IOException]::new('ProgramData is unavailable for OneDrive removal logging')
    }
    $logDirectory = [System.IO.Path]::Combine($env:ProgramData, 'LetRecovery', 'Logs')
    $logPath = [System.IO.Path]::Combine($logDirectory, 'OneDrive-win32-removal.json')
    $temporaryLogPath = $logPath + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
} catch {
    Write-SetupWarning -Code 'log_path_initialization_failed' -ExceptionType $_.Exception.GetType().FullName -HResult $_.Exception.HResult
}

function Get-OneDriveInventory {
    $registryEntries = [System.Collections.Generic.List[object]]::new()
    $uninstallKeys = @(
        'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\OneDriveSetup.exe',
        'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\OneDriveSetup.exe',
        'Registry::HKEY_CURRENT_USER\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\OneDriveSetup.exe'
    )
    foreach ($key in $uninstallKeys) {
        if (Test-Path -LiteralPath $key -PathType Container) {
            $properties = Get-ItemProperty -LiteralPath $key -ErrorAction Stop
            $registryEntries.Add([ordered]@{
                scope = if ($key.StartsWith('Registry::HKEY_LOCAL_MACHINE', [System.StringComparison]::Ordinal)) { 'machine' } else { 'current_system_profile' }
                path = $key
                display_name = [string]$properties.DisplayName
                display_version = [string]$properties.DisplayVersion
                install_location = [string]$properties.InstallLocation
                uninstall_string = [string]$properties.UninstallString
            })
        }
    }

    $executableEntries = [System.Collections.Generic.List[object]]::new()
    $executableRoots = [System.Collections.Generic.List[object]]::new()
    if (-not [string]::IsNullOrWhiteSpace($env:ProgramFiles)) {
        $executableRoots.Add([pscustomobject]@{ scope = 'machine'; path = [System.IO.Path]::Combine($env:ProgramFiles, 'Microsoft OneDrive') })
    }
    $programFilesX86 = [System.Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    if (-not [string]::IsNullOrWhiteSpace($programFilesX86)) {
        $executableRoots.Add([pscustomobject]@{ scope = 'machine'; path = [System.IO.Path]::Combine($programFilesX86, 'Microsoft OneDrive') })
    }
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        $executableRoots.Add([pscustomobject]@{ scope = 'current_system_profile'; path = [System.IO.Path]::Combine($env:LOCALAPPDATA, 'Microsoft', 'OneDrive') })
    }

    $seenRoots = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($rootCandidate in $executableRoots) {
        $rootPath = [System.IO.Path]::GetFullPath([string]$rootCandidate.path).TrimEnd('\')
        if (-not $seenRoots.Add($rootPath) -or -not (Test-Path -LiteralPath $rootPath -PathType Container)) {
            continue
        }
        $rootItem = Get-Item -LiteralPath $rootPath -Force -ErrorAction Stop
        if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            $executableEntries.Add([ordered]@{
                scope = $rootCandidate.scope
                path = $rootPath
                reparse_point = $true
                length = $null
                reason = 'fixed_inventory_root_is_reparse_point'
            })
            continue
        }

        $directories = [System.Collections.Generic.List[object]]::new()
        $directories.Add($rootItem)
        foreach ($child in $rootItem.EnumerateDirectories()) {
            if ($directories.Count -ge 129) {
                throw [System.IO.IOException]::new('OneDrive fixed inventory exceeds the direct-child limit')
            }
            if ($child.Name.Length -eq 0 -or $child.Name.Length -gt 80) {
                throw [System.IO.IOException]::new('OneDrive fixed inventory contains an invalid direct-child name')
            }
            if (($child.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                $executableEntries.Add([ordered]@{
                    scope = $rootCandidate.scope
                    path = [System.IO.Path]::GetFullPath($child.FullName)
                    reparse_point = $true
                    length = $null
                    reason = 'fixed_inventory_direct_child_is_reparse_point'
                })
                continue
            }
            $directories.Add($child)
        }

        foreach ($directory in $directories) {
            $path = [System.IO.Path]::Combine($directory.FullName, 'OneDrive.exe')
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
                $executableEntries.Add([ordered]@{
                    scope = $rootCandidate.scope
                    path = [System.IO.Path]::GetFullPath($item.FullName)
                    reparse_point = (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)
                    length = $item.Length
                })
            }
        }
    }

    return [pscustomobject][ordered]@{
        registry_entries = @($registryEntries.ToArray())
        executable_entries = @($executableEntries.ToArray())
        current_system_evidence_count = $registryEntries.Count + $executableEntries.Count
    }
}

function Get-TrustedOneDriveSetupEvidence {
    param([Parameter(Mandatory = $true)][string]$Path)

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw [System.IO.IOException]::new('OneDriveSetup candidate is not a regular non-reparse file')
    }
    $actualPath = [System.IO.Path]::GetFullPath($item.FullName)
    if (-not [string]::Equals($actualPath, $Path, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw [System.IO.IOException]::new('OneDriveSetup candidate path changed during inspection')
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $Path -ErrorAction Stop
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or $null -eq $signature.SignerCertificate) {
        throw [System.Security.SecurityException]::new('OneDriveSetup Authenticode signature is not valid')
    }
    $simpleName = $signature.SignerCertificate.GetNameInfo(
        [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName,
        $false
    )
    $isMicrosoftSigner =
        [string]::Equals($simpleName, 'Microsoft Windows', [System.StringComparison]::Ordinal) -or
        [string]::Equals($simpleName, 'Microsoft Corporation', [System.StringComparison]::Ordinal)
    if (-not $isMicrosoftSigner) {
        throw [System.Security.SecurityException]::new('OneDriveSetup signer is not an approved Microsoft signer')
    }

    $chain = [System.Security.Cryptography.X509Certificates.X509Chain]::new()
    try {
        $chain.ChainPolicy.RevocationMode = [System.Security.Cryptography.X509Certificates.X509RevocationMode]::NoCheck
        # Get-AuthenticodeSignature already validates the timestamped Authenticode signature. The
        # separate chain build is only for deterministic root pinning, so an expired leaf that was
        # validly timestamped may still be walked to its Microsoft root.
        $chain.ChainPolicy.VerificationFlags = [System.Security.Cryptography.X509Certificates.X509VerificationFlags]::IgnoreNotTimeValid
        if (-not $chain.Build($signature.SignerCertificate) -or $chain.ChainElements.Count -lt 2) {
            throw [System.Security.SecurityException]::new('OneDriveSetup signer chain could not be built for root pinning')
        }
        $root = $chain.ChainElements[$chain.ChainElements.Count - 1].Certificate
        if (-not [string]::Equals($root.Subject, $root.Issuer, [System.StringComparison]::Ordinal)) {
            throw [System.Security.SecurityException]::new('OneDriveSetup signer chain does not terminate in a self-issued root')
        }
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            $rootHashBytes = $sha256.ComputeHash($root.RawData)
        } finally {
            $sha256.Dispose()
        }
        $rootHash = [System.BitConverter]::ToString($rootHashBytes).Replace('-', '')
        if (-not [string]::Equals($rootHash, 'DF545BF919A2439C36983B54CDFC903DFA4F37D3996D8D84B4C31EEC6F3C163E', [System.StringComparison]::Ordinal)) {
            throw [System.Security.SecurityException]::new('OneDriveSetup signer chain is not rooted in the pinned Microsoft Root CA 2010')
        }
    } finally {
        $chain.Dispose()
    }

    $hash = Get-FileHash -LiteralPath $Path -Algorithm SHA256 -ErrorAction Stop
    return [pscustomobject][ordered]@{
        path = $actualPath
        signer = $simpleName
        sha256 = ([string]$hash.Hash).ToUpperInvariant()
        length = $item.Length
    }
}

try {
    $before = Get-OneDriveInventory
    $result.before = $before

    $windowsRoot = [System.IO.Path]::GetFullPath($env:SystemRoot).TrimEnd('\')
    $system32Candidate = [System.IO.Path]::Combine($windowsRoot, 'System32', 'OneDriveSetup.exe')
    $sysWow64Candidate = [System.IO.Path]::Combine($windowsRoot, 'SysWOW64', 'OneDriveSetup.exe')
    if ([Environment]::Is64BitOperatingSystem) {
        # Microsoft's x64 deployment guidance names SysWOW64. System32 remains the fixed fallback
        # when that preferred candidate is missing or fails trust validation before any launch.
        $candidatePaths = @($sysWow64Candidate, $system32Candidate)
    } else {
        $candidatePaths = @($system32Candidate)
    }
    $trustedCandidates = [System.Collections.Generic.List[object]]::new()
    $presentCandidateCount = 0
    foreach ($candidatePath in $candidatePaths) {
        if (-not (Test-Path -LiteralPath $candidatePath -PathType Leaf)) {
            $result.candidates.Add([ordered]@{ path = $candidatePath; status = 'missing' })
            continue
        }
        $presentCandidateCount++
        try {
            $evidence = Get-TrustedOneDriveSetupEvidence -Path $candidatePath
            $trustedCandidates.Add($evidence)
            $result.candidates.Add([ordered]@{
                path = $evidence.path
                status = 'trusted'
                signer = $evidence.signer
                sha256 = $evidence.sha256
                length = $evidence.length
            })
        } catch {
            $result.candidates.Add([ordered]@{
                path = $candidatePath
                status = 'warning'
                reason = 'candidate_trust_validation_failed'
                exception_type = $_.Exception.GetType().FullName
                hresult = $_.Exception.HResult
            })
        }
    }

    if ($trustedCandidates.Count -eq 0) {
        $result.after = Get-OneDriveInventory
        if ($presentCandidateCount -eq 0 -and $result.before.current_system_evidence_count -eq 0 -and $result.after.current_system_evidence_count -eq 0) {
            $result.status = 'completed'
            $result.reason = 'official_uninstaller_absent_and_current_system_fixed_inventory_not_present'
        } else {
            $result.reason = 'no_trusted_official_uninstaller'
        }
    } else {
        # Never launch a second setup executable after the first launch. OneDriveSetup can create
        # descendants, and PowerShell documents that Start-Process -Wait waits for the whole tree.
        # A second fallback launch while the first tree is still settling would add interference.
        $selected = $trustedCandidates[0]
        $attempt = [ordered]@{
            path = $selected.path
            initial_sha256 = $selected.sha256
            prelaunch_sha256 = $null
            exit_code = $null
            fresh_evidence_count = $null
            timeout_milliseconds = $operationTimeoutMilliseconds
            timed_out = $false
            terminated_after_timeout = $false
            status = 'warning'
            reason = 'attempt_not_started'
            exception_type = $null
            hresult = $null
        }
        $result.selected_path = $selected.path
        $result.selected_sha256 = $selected.sha256
        $process = $null

        try {
            # Re-open, revalidate the signature, and compare a fresh hash immediately before the
            # only launch. A fallback candidate never inherits trust from another candidate.
            $prelaunch = Get-TrustedOneDriveSetupEvidence -Path $selected.path
            $attempt.prelaunch_sha256 = $prelaunch.sha256
            if (-not [string]::Equals($prelaunch.sha256, $selected.sha256, [System.StringComparison]::Ordinal)) {
                throw [System.Security.SecurityException]::new('OneDriveSetup changed after trust validation')
            }

            Write-Host ('[LetRecovery] OneDrive cleanup: running the official uninstaller (timeout {0} seconds)...' -f [int]($operationTimeoutMilliseconds / 1000))
            $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
            # Deliberately omit Start-Process -Wait: Microsoft documents that -Wait follows the
            # entire descendant tree and can therefore retain this Setup console indefinitely.
            $process = Start-Process -FilePath $selected.path -ArgumentList @('/uninstall') -PassThru -WindowStyle Hidden
            $remainingMilliseconds = $operationTimeoutMilliseconds - [int]$stopwatch.ElapsedMilliseconds
            $exited = $remainingMilliseconds -gt 0 -and $process.WaitForExit([int]$remainingMilliseconds)
            if (-not $exited) {
                $attempt.timed_out = $true
                $attempt.reason = 'official_uninstaller_timeout'
                $timeoutException = [System.TimeoutException]::new('OneDriveSetup exceeded the bounded specialize timeout')
                $attempt.exception_type = $timeoutException.GetType().FullName
                $attempt.hresult = $timeoutException.HResult
                $result.reason = $attempt.reason
                $result.exception_type = $attempt.exception_type
                $result.hresult = $attempt.hresult
                try {
                    if (-not $process.HasExited) {
                        $process.Kill()
                        $attempt.terminated_after_timeout = $process.WaitForExit(5000)
                    }
                } catch {
                    Write-SetupWarning -Code 'timed_out_process_termination_failed' -ExceptionType $_.Exception.GetType().FullName -HResult $_.Exception.HResult
                }
                Write-SetupWarning -Code 'official_uninstaller_timeout' -ExceptionType $attempt.exception_type -HResult $attempt.hresult
            } else {
                $attempt.exit_code = $process.ExitCode
                $result.uninstaller_exit_code = $process.ExitCode

                # A direct process may exit before its descendants finish. Poll only the fixed
                # typed inventory within the same total deadline; never wait on arbitrary children.
                do {
                    $result.after = Get-OneDriveInventory
                    $attempt.fresh_evidence_count = $result.after.current_system_evidence_count
                    if ($attempt.fresh_evidence_count -eq 0) {
                        break
                    }
                    $remainingMilliseconds = $operationTimeoutMilliseconds - [int]$stopwatch.ElapsedMilliseconds
                    if ($remainingMilliseconds -le 0) {
                        break
                    }
                    Start-Sleep -Milliseconds ([Math]::Min(1000, $remainingMilliseconds))
                } while ($true)

                if ($process.ExitCode -eq 0 -and $attempt.fresh_evidence_count -eq 0) {
                    $attempt.status = 'completed'
                    $attempt.reason = 'official_uninstaller_succeeded_and_current_system_fixed_inventory_not_present'
                    $result.status = 'completed'
                    $result.reason = $attempt.reason
                    $result.exception_type = $null
                    $result.hresult = $null
                } elseif ($process.ExitCode -ne 0) {
                    $attempt.reason = 'official_uninstaller_nonzero_exit'
                    $result.reason = if ($attempt.fresh_evidence_count -gt 0) {
                        'fresh_readback_still_has_onedrive_inventory'
                    } else {
                        'fixed_inventory_absent_but_uninstaller_failed'
                    }
                } else {
                    $attempt.reason = 'fresh_readback_still_has_onedrive_inventory'
                    $result.reason = $attempt.reason
                }
            }
        } catch {
            $attempt.reason = 'candidate_attempt_failed'
            $attempt.exception_type = $_.Exception.GetType().FullName
            $attempt.hresult = $_.Exception.HResult
            $result.reason = $attempt.reason
            $result.exception_type = $attempt.exception_type
            $result.hresult = $attempt.hresult
        } finally {
            if ($null -ne $process) {
                $process.Close()
            }
        }
        $result.attempts.Add($attempt)
    }
} catch {
    $result.status = 'warning'
    if ($result.reason -eq 'not_started') {
        $result.reason = 'online_operation_failed'
    }
    $result.exception_type = $_.Exception.GetType().FullName
    $result.hresult = $_.Exception.HResult
}

if ($result.status -eq 'completed') {
    Write-Host '[LetRecovery] OneDrive cleanup: completed.'
} else {
    Write-Host ('[LetRecovery] OneDrive cleanup: warning ({0}); Windows Setup will continue.' -f $result.reason)
}

if ($null -ne $logPath -and $null -ne $temporaryLogPath) {
    try {
        [void][System.IO.Directory]::CreateDirectory($logDirectory)
        $json = ConvertTo-Json -InputObject $result -Depth 8 -Compress
        [System.IO.File]::WriteAllText($temporaryLogPath, $json, [System.Text.UTF8Encoding]::new($false))
        Move-Item -LiteralPath $temporaryLogPath -Destination $logPath -Force
    } catch {
        Write-SetupWarning -Code 'structured_log_persist_failed' -ExceptionType $_.Exception.GetType().FullName -HResult $_.Exception.HResult
    }
}
exit 0
"#;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptDisposition {
    Completed,
    WarningAfterNonzeroExit,
    WarningAfterResidualInventory,
    WarningAfterTimeout,
}

/// Pure policy oracle for the specialize script's candidate loop. The script-text regression test
/// below also pins the equivalent PowerShell predicates so this truth table cannot silently drift
/// away from the staged implementation.
#[cfg(test)]
fn classify_attempt(
    timed_out: bool,
    exit_code: Option<i32>,
    fresh_evidence_count: usize,
) -> AttemptDisposition {
    if timed_out {
        AttemptDisposition::WarningAfterTimeout
    } else if exit_code == Some(0) && fresh_evidence_count == 0 {
        AttemptDisposition::Completed
    } else if exit_code != Some(0) {
        AttemptDisposition::WarningAfterNonzeroExit
    } else {
        AttemptDisposition::WarningAfterResidualInventory
    }
}

pub fn online_script_path(target_partition: &str) -> Result<PathBuf> {
    let root = normalized_target_root(target_partition)?;
    Ok(root
        .join("LetRecovery_Scripts")
        .join(ONLINE_SCRIPT_FILE_NAME))
}

/// Atomically stage the fixed script and verify the published bytes.
pub fn stage_online_removal_script(target_partition: &str) -> Result<PathBuf> {
    let target = online_script_path(target_partition)?;
    let directory = target
        .parent()
        .context("OneDrive online script has no parent directory")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create OneDrive script directory {}", directory.display()))?;
    reject_reparse_or_non_directory(directory)?;

    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-onedrive",
        "ps1",
        ONLINE_REMOVAL_SCRIPT.as_bytes(),
    )
    .context("stage OneDrive online removal script")?;
    if std::fs::read(temporary.path()).context("read back staged OneDrive script")?
        != ONLINE_REMOVAL_SCRIPT.as_bytes()
    {
        anyhow::bail!("staged OneDrive online script readback mismatch");
    }
    temporary
        .persist_replace(&target)
        .with_context(|| format!("publish OneDrive online script {}", target.display()))?;
    if !online_script_is_staged(target_partition)? {
        anyhow::bail!("published OneDrive online script readback mismatch");
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
            "OneDrive online script is not a regular file: {}",
            path.display()
        );
    }
    Ok(
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))?
            == ONLINE_REMOVAL_SCRIPT.as_bytes(),
    )
}

/// Render the fixed warning-only specialize command. Callers must include it only in a built-in
/// Windows 10/11 answer file after the script has passed byte-for-byte staging verification.
pub fn render_specialize_command(order: u32) -> Result<String> {
    let path = format!(
        r#"powershell.exe -NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "%SystemDrive%\LetRecovery_Scripts\{ONLINE_SCRIPT_FILE_NAME}""#
    );
    crate::unattend_command::render_specialize_run_synchronous_command(
        order,
        &path,
        "Remove Win32 OneDrive sync client",
    )
}

fn normalized_target_root(target_partition: &str) -> Result<PathBuf> {
    let value = target_partition.trim().trim_end_matches(['\\', '/']);
    let bytes = value.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        anyhow::bail!("target partition must be a drive letter, got {target_partition:?}");
    }
    let root = PathBuf::from(format!("{}\\", value.to_ascii_uppercase()));
    if !root.join("Windows\\System32\\ntdll.dll").is_file() {
        anyhow::bail!("target does not contain a complete offline Windows directory");
    }
    Ok(root)
}

fn reject_reparse_or_non_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect OneDrive script directory {}", path.display()))?;
    if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!(
            "OneDrive script directory is not a regular directory: {}",
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
    fn script_uses_only_official_fixed_uninstaller_and_readback_boundaries() {
        assert!(ONLINE_REMOVAL_SCRIPT.contains("'System32', 'OneDriveSetup.exe'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("'SysWOW64', 'OneDriveSetup.exe'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Get-AuthenticodeSignature -LiteralPath"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("SignatureStatus]::Valid"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("'Microsoft Windows'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("'Microsoft Corporation'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains(
            crate::driver_trust::MICROSOFT_ROOT_CA_2010_SHA256
                .to_ascii_uppercase()
                .as_str()
        ));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("X509Chain]::new()"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("root.Subject, $root.Issuer"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Get-FileHash -LiteralPath"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("-ArgumentList @('/uninstall')"));
        assert!(!ONLINE_REMOVAL_SCRIPT.contains("-Wait -PassThru"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$process.WaitForExit([int]$remainingMilliseconds)"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$operationTimeoutMilliseconds = 120000"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$process.Kill()"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Uninstall\\OneDriveSetup.exe"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("'Microsoft OneDrive'"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains(".EnumerateDirectories()"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("direct-child limit"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$directories.Count -ge 129"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$child.Name.Length -gt 80"));
        assert!(ONLINE_REMOVAL_SCRIPT
            .contains("[System.IO.Path]::Combine($directory.FullName, 'OneDrive.exe')"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("fresh_readback_still_has_onedrive_inventory"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("future_user_registration_proven_absent = $false"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$selected = $trustedCandidates[0]"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$presentCandidateCount++"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("if ($presentCandidateCount -eq 0 -and"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("$result.attempts.Add($attempt)"));
        assert!(ONLINE_REMOVAL_SCRIPT
            .contains("if ($process.ExitCode -eq 0 -and $attempt.fresh_evidence_count -eq 0)"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("elseif ($process.ExitCode -ne 0)"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("official_uninstaller_timeout"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("Windows Setup will continue"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("candidate_attempt_failed"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("exit 0"));
    }

    #[test]
    fn attempt_policy_never_retries_after_launch_and_only_completes_after_fresh_success() {
        assert_eq!(
            classify_attempt(false, Some(0), 0),
            AttemptDisposition::Completed
        );
        assert_eq!(
            classify_attempt(false, Some(1), 0),
            AttemptDisposition::WarningAfterNonzeroExit
        );
        assert_eq!(
            classify_attempt(false, Some(1), 2),
            AttemptDisposition::WarningAfterNonzeroExit
        );
        assert_eq!(
            classify_attempt(false, Some(0), 1),
            AttemptDisposition::WarningAfterResidualInventory
        );
        assert_eq!(
            classify_attempt(true, None, 1),
            AttemptDisposition::WarningAfterTimeout
        );
    }

    #[test]
    fn logging_failures_emit_fixed_setup_warning_without_changing_exit_policy() {
        assert!(ONLINE_REMOVAL_SCRIPT.contains("[Console]::Error.WriteLine("));
        assert!(ONLINE_REMOVAL_SCRIPT
            .contains("LETRECOVERY_ONEDRIVE_WARNING schema=LetRecovery.OneDriveWin32Removal.v1"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("log_path_initialization_failed"));
        assert!(ONLINE_REMOVAL_SCRIPT.contains("structured_log_persist_failed"));
        assert!(ONLINE_REMOVAL_SCRIPT.trim_end().ends_with("exit 0"));
    }

    #[test]
    fn script_has_no_search_or_destructive_fallback() {
        for forbidden in [
            "Get-ChildItem",
            "Remove-Item",
            "Remove-ItemProperty",
            "reg.exe",
            "where.exe",
            "Win32_Product",
            "PATH",
            "taskkill",
            "Stop-Process",
            "Microsoft OneDrive*",
        ] {
            assert!(!ONLINE_REMOVAL_SCRIPT.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn specialize_hook_is_fixed_xml_and_warning_only() {
        assert!(render_specialize_command(0).is_err());
        let command = render_specialize_command(5).unwrap();
        assert!(command.contains("<Order>5</Order>"));
        assert!(command.contains(ONLINE_SCRIPT_FILE_NAME));
        assert!(command.contains("-WindowStyle Hidden"));
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
}
