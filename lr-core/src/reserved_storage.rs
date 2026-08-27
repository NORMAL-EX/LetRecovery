//! Supported, warning-only reserved-storage control for a newly installed Windows image.
//!
//! Microsoft documents `/Set-ReservedStorageState` and `/Get-ReservedStorageState` as online-only
//! operations available starting with Windows 10 version 2004. Consequently this module never
//! edits ReserveManager's implementation-detail registry values in an offline image. It stages a
//! fixed script that may only be wired into LetRecovery's built-in Win10/11 specialize pass.
//!
//! Microsoft references checked for this boundary:
//! <https://learn.microsoft.com/windows-hardware/manufacture/desktop/dism-storage-reserve>
//! <https://learn.microsoft.com/powershell/module/dism/get-windowsreservedstoragestate>
//! <https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/start-process?view=powershell-5.1>
//! <https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.process.waitforexit?view=netframework-4.8>

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const ONLINE_SCRIPT_FILE_NAME: &str = "disable-reserved-storage.ps1";
pub const MINIMUM_SUPPORTED_BUILD: u32 = 19_041;

/// Opaque proof that a target version satisfies Microsoft's online Reserved Storage boundary.
/// Callers cannot construct this token without passing the centralized version gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportedTargetVersion {
    build: u32,
}

impl SupportedTargetVersion {
    pub const fn new(major: u32, minor: u32, build: u32) -> Option<Self> {
        if is_supported_target_version(major, minor, build) {
            Some(Self { build })
        } else {
            None
        }
    }

    pub const fn build(self) -> u32 {
        self.build
    }
}

/// Fixed best-effort online operation. The two DISM.exe calls use Microsoft's documented
/// online-only commands. Semantic readback uses the typed DISM PowerShell object rather than
/// parsing localized command output. Every feature-level failure is recorded and exits zero so
/// Windows Setup is never blocked by this optional optimization.
const ONLINE_DISABLE_SCRIPT: &str = r#"[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$operationTimeoutMilliseconds = 120000
$result = [ordered]@{
    schema = 'LetRecovery.ReservedStorage.v1'
    status = 'warning'
    reason = 'not_started'
    os_build = [System.Environment]::OSVersion.Version.Build
    set_exit_code = $null
    get_exit_code = $null
    readback_state = $null
    exception_type = $null
    hresult = $null
}
Write-Host '[LetRecovery] Reserved Storage: checking the target Windows version...'
$logDirectory = [System.IO.Path]::Combine($env:ProgramData, 'LetRecovery', 'Logs')
$logPath = [System.IO.Path]::Combine($logDirectory, 'ReservedStorage-disable.json')
$temporaryLogPath = $logPath + '.tmp'

function Invoke-BoundedDism {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$Stage,
        [Parameter(Mandatory = $true)][System.Diagnostics.Stopwatch]$Stopwatch
    )

    $remainingMilliseconds = $operationTimeoutMilliseconds - [int]$Stopwatch.ElapsedMilliseconds
    if ($remainingMilliseconds -le 0) {
        throw [System.TimeoutException]::new('Reserved Storage operation exceeded the bounded specialize timeout')
    }

    Write-Host ('[LetRecovery] Reserved Storage: {0} (remaining timeout {1} seconds)...' -f $Stage, [int][Math]::Ceiling($remainingMilliseconds / 1000.0))
    $process = $null
    try {
        # Do not use Start-Process -Wait. Microsoft documents that it follows the entire process
        # tree; waiting only for this fixed DISM process keeps this optional hook bounded.
        $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit([int]$remainingMilliseconds)) {
            try {
                if (-not $process.HasExited) {
                    $process.Kill()
                    [void]$process.WaitForExit(5000)
                }
            } catch {
            }
            throw [System.TimeoutException]::new('DISM exceeded the bounded Reserved Storage timeout')
        }
        return $process.ExitCode
    } finally {
        if ($null -ne $process) {
            $process.Close()
        }
    }
}

try {
    $version = [System.Environment]::OSVersion.Version
    if ($version.Major -ne 10 -or $version.Build -lt 19041) {
        $result.reason = 'unsupported_online_os_version'
    } else {
        $dismPath = [System.IO.Path]::Combine($env:SystemRoot, 'System32', 'dism.exe')
        if (-not [System.IO.File]::Exists($dismPath)) {
            throw [System.IO.FileNotFoundException]::new('DISM executable is unavailable', $dismPath)
        }

        $operationStopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        $result.set_exit_code = Invoke-BoundedDism -FilePath $dismPath -Arguments @('/Online', '/Set-ReservedStorageState', '/State:Disabled', '/Quiet', '/NoRestart') -Stage 'disabling reserved storage' -Stopwatch $operationStopwatch
        if ($result.set_exit_code -ne 0) {
            $result.reason = 'set_command_failed'
        } else {
            $result.get_exit_code = Invoke-BoundedDism -FilePath $dismPath -Arguments @('/Online', '/Get-ReservedStorageState', '/Quiet', '/NoRestart') -Stage 'verifying the DISM state' -Stopwatch $operationStopwatch
            if ($result.get_exit_code -ne 0) {
                $result.reason = 'get_command_failed'
            } else {
                Import-Module -Name Dism -ErrorAction Stop
                $stateObject = Get-WindowsReservedStorageState -ErrorAction Stop
                $stateProperty = $stateObject.PSObject.Properties['ReservedStorageState']
                if ($null -eq $stateProperty) {
                    $result.reason = 'typed_readback_missing_state'
                } else {
                    $state = [string]$stateProperty.Value
                    $result.readback_state = $state
                    if ([string]::Equals($state, 'Disabled', [System.StringComparison]::OrdinalIgnoreCase)) {
                        $result.status = 'completed'
                        $result.reason = 'set_succeeded_and_typed_readback_disabled'
                    } else {
                        $result.reason = 'typed_readback_not_disabled'
                    }
                }
            }
        }
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
    Write-Host '[LetRecovery] Reserved Storage: completed.'
} else {
    Write-Host ('[LetRecovery] Reserved Storage: warning ({0}); Windows Setup will continue.' -f $result.reason)
}

try {
    [void][System.IO.Directory]::CreateDirectory($logDirectory)
    $json = ConvertTo-Json -InputObject $result -Depth 4 -Compress
    [System.IO.File]::WriteAllText($temporaryLogPath, $json, [System.Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporaryLogPath -Destination $logPath -Force
} catch {
    try {
        [Console]::Error.WriteLine(
            ('LETRECOVERY_RESERVED_STORAGE_WARNING schema=LetRecovery.ReservedStorage.v1 code=structured_log_persist_failed exception_type={0} hresult={1}' -f
                $_.Exception.GetType().FullName,
                $_.Exception.HResult)
        )
    } catch {
    }
}
exit 0
"#;

pub const fn is_supported_target_version(major: u32, minor: u32, build: u32) -> bool {
    major == 10 && minor == 0 && build >= MINIMUM_SUPPORTED_BUILD
}

pub fn online_script_path(target_partition: &str) -> Result<PathBuf> {
    let root = normalized_target_root(target_partition)?;
    Ok(root
        .join("LetRecovery_Scripts")
        .join(ONLINE_SCRIPT_FILE_NAME))
}

/// Atomically stage and byte-for-byte verify the fixed script.
pub fn stage_online_disable_script(target_partition: &str) -> Result<PathBuf> {
    let target = online_script_path(target_partition)?;
    let directory = target
        .parent()
        .context("reserved-storage online script has no parent directory")?;
    std::fs::create_dir_all(directory).with_context(|| {
        format!(
            "create reserved-storage script directory {}",
            directory.display()
        )
    })?;
    reject_reparse_or_non_directory(directory)?;

    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-reserved-storage",
        "ps1",
        ONLINE_DISABLE_SCRIPT.as_bytes(),
    )
    .context("stage reserved-storage online script")?;
    if std::fs::read(temporary.path()).context("read back staged reserved-storage script")?
        != ONLINE_DISABLE_SCRIPT.as_bytes()
    {
        anyhow::bail!("staged reserved-storage script readback mismatch");
    }
    temporary.persist_replace(&target).with_context(|| {
        format!(
            "publish reserved-storage online script {}",
            target.display()
        )
    })?;
    if !online_script_is_staged(target_partition)? {
        anyhow::bail!("published reserved-storage script readback mismatch");
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
            "reserved-storage online script is not a regular file: {}",
            path.display()
        );
    }
    Ok(
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))?
            == ONLINE_DISABLE_SCRIPT.as_bytes(),
    )
}

/// Render the fixed specialize command. Callers must only include it for a confirmed supported
/// target and a built-in answer file.
pub fn render_specialize_command(order: u32) -> Result<String> {
    let path = format!(
        r#"powershell.exe -NoP -NonI -WindowStyle Hidden -EP Bypass -File "%SystemDrive%\LetRecovery_Scripts\{ONLINE_SCRIPT_FILE_NAME}""#
    );
    crate::unattend_command::render_specialize_run_synchronous_command(
        order,
        &path,
        "Disable reserved storage",
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
    let metadata = std::fs::symlink_metadata(path).with_context(|| {
        format!(
            "inspect reserved-storage script directory {}",
            path.display()
        )
    })?;
    if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!(
            "reserved-storage script directory is not a regular directory: {}",
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
    fn version_gate_matches_microsoft_minimum() {
        assert!(!is_supported_target_version(10, 0, 18_363));
        assert!(!is_supported_target_version(10, 0, 19_040));
        assert!(is_supported_target_version(10, 0, 19_041));
        assert!(is_supported_target_version(10, 0, 26_100));
        assert!(!is_supported_target_version(10, 1, 26_100));
        assert!(!is_supported_target_version(6, 3, 19_041));
        assert!(SupportedTargetVersion::new(10, 0, 19_040).is_none());
        assert_eq!(
            SupportedTargetVersion::new(10, 0, 19_041)
                .expect("Windows 10 2004 is supported")
                .build(),
            19_041
        );
    }

    #[test]
    fn script_uses_supported_online_dism_and_typed_readback() {
        assert!(ONLINE_DISABLE_SCRIPT.contains("'/Set-ReservedStorageState'"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("'/State:Disabled'"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("'/Get-ReservedStorageState'"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("Get-WindowsReservedStorageState"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("Properties['ReservedStorageState']"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("$operationTimeoutMilliseconds = 120000"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("$process.WaitForExit([int]$remainingMilliseconds)"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("$process.Kill()"));
        assert!(!ONLINE_DISABLE_SCRIPT.contains("-Wait -PassThru"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("Windows Setup will continue"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("code=structured_log_persist_failed"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("[Console]::Error.WriteLine"));
        assert!(ONLINE_DISABLE_SCRIPT.contains("exit 0"));
        assert!(!ONLINE_DISABLE_SCRIPT.contains("ShippedWithReserves"));
        assert!(!ONLINE_DISABLE_SCRIPT.contains("PassedPolicy"));
        assert!(!ONLINE_DISABLE_SCRIPT.contains("ReserveManager"));
    }

    #[test]
    fn specialize_hook_is_fixed_and_rejects_zero_order() {
        assert!(render_specialize_command(0).is_err());
        let command = render_specialize_command(4).unwrap();
        assert!(command.contains("<Order>4</Order>"));
        assert!(command.contains(ONLINE_SCRIPT_FILE_NAME));
        assert!(command.contains("powershell.exe -NoP -NonI -WindowStyle Hidden -EP Bypass"));
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
